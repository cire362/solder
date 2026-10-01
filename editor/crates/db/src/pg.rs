use std::collections::HashMap;

use tokio_postgres::{Client, SimpleQueryMessage, types::Type};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::{
    Column, ColumnInfo, ConnectionSpec, ForeignKey, Index, Object, ObjectKind, QueryResult, Result,
    Schema, Value,
    params::{self, Tls},
    tls,
};

pub struct Pg {
    client: Client,
    prepare: bool,
    /// The client pipelines requests; a transaction must not interleave
    /// with other queries on the same connection.
    busy: tokio::sync::Mutex<()>,
}

impl Pg {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        let params = params::postgres(&spec.url)?;
        let config: tokio_postgres::Config = params.url.parse().map_err(|e| format!("{e}"))?;
        let tls = MakeRustlsConnect::new(match &params.tls {
            Tls::Verified(root) => tls::verified(root.as_deref())?,
            Tls::Off | Tls::Unverified => tls::unverified(),
        });
        let (client, connection) = config.connect(tls).await.map_err(error)?;
        tokio::spawn(connection);
        if spec.read_only {
            client
                .batch_execute("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
                .await
                .map_err(error)?;
        }
        Ok(Self {
            client,
            prepare: params.prepare,
            busy: tokio::sync::Mutex::new(()),
        })
    }

    /// The simple protocol runs any statement and returns text; preparing
    /// the statement first (without running it) tells the column types, so
    /// numbers, booleans and JSON are shown as such.
    pub async fn query(&self, text: &str) -> Result<QueryResult> {
        let _busy = self.busy.lock().await;
        let types: Option<Vec<Type>> = if self.prepare {
            self.client
                .prepare(text)
                .await
                .ok()
                .map(|s| s.columns().iter().map(|c| c.type_().clone()).collect())
        } else {
            None
        };
        let messages = self.client.simple_query(text).await.map_err(error)?;
        let mut result = QueryResult::default();
        for message in messages {
            match message {
                // Several statements (a selection): the last result wins.
                SimpleQueryMessage::RowDescription(columns) => {
                    result = QueryResult::default();
                    result.columns = columns
                        .iter()
                        .enumerate()
                        .map(|(i, c)| Column {
                            name: c.name().to_string(),
                            type_name: types
                                .as_ref()
                                .and_then(|t| t.get(i))
                                .map_or_else(String::new, |t| t.name().to_string()),
                        })
                        .collect();
                }
                SimpleQueryMessage::Row(row) => {
                    let values = (0..row.len())
                        .map(|i| {
                            let ty = types.as_ref().and_then(|t| t.get(i));
                            row.get(i).map_or(Value::Null, |s| decode(s, ty))
                        })
                        .collect();
                    result.push_row(values);
                }
                SimpleQueryMessage::CommandComplete(affected) if result.columns.is_empty() => {
                    result.affected = Some(affected);
                }
                _ => {}
            }
        }
        Ok(result)
    }

    pub async fn apply(&self, statements: &[String]) -> Result<()> {
        let _busy = self.busy.lock().await;
        self.client.batch_execute("BEGIN").await.map_err(error)?;
        for (i, statement) in statements.iter().enumerate() {
            let affected = match self.client.simple_query(statement).await {
                Ok(messages) => messages
                    .iter()
                    .find_map(|m| match m {
                        SimpleQueryMessage::CommandComplete(n) => Some(*n),
                        _ => None,
                    })
                    .unwrap_or(0),
                Err(e) => {
                    let _ = self.client.batch_execute("ROLLBACK").await;
                    return Err(format!("{} Nothing was saved.", error(e)));
                }
            };
            if affected != 1 {
                let _ = self.client.batch_execute("ROLLBACK").await;
                return Err(crate::unmatched(i, statements.len(), affected));
            }
        }
        self.client.batch_execute("COMMIT").await.map_err(error)
    }

    pub async fn apply_ddl(&self, statements: &[String]) -> Result<()> {
        let _busy = self.busy.lock().await;
        self.client.batch_execute("BEGIN").await.map_err(error)?;
        for (i, statement) in statements.iter().enumerate() {
            if let Err(e) = self.client.batch_execute(statement).await {
                let _ = self.client.batch_execute("ROLLBACK").await;
                return Err(format!(
                    "Statement {} of {}: {}. Nothing was changed.",
                    i + 1,
                    statements.len(),
                    error(e)
                ));
            }
        }
        self.client.batch_execute("COMMIT").await.map_err(error)
    }

    pub async fn schema(&self) -> Result<Schema> {
        let _busy = self.busy.lock().await;
        let columns = self
            .rows(
                "SELECT c.table_schema, c.table_name, t.table_type, c.column_name, \
                   (SELECT format_type(a.atttypid, a.atttypmod) FROM pg_attribute a \
                     JOIN pg_class cl ON cl.oid = a.attrelid JOIN pg_namespace n ON n.oid = cl.relnamespace \
                     WHERE n.nspname = c.table_schema AND cl.relname = c.table_name \
                       AND a.attname = c.column_name), \
                   c.is_nullable, \
                   coalesce(c.column_default, ''), \
                   CASE WHEN c.is_identity = 'YES' OR c.column_default LIKE 'nextval(%' \
                     OR c.is_generated = 'ALWAYS' THEN '1' ELSE '' END \
                 FROM information_schema.columns c \
                 JOIN information_schema.tables t USING (table_schema, table_name) \
                 WHERE c.table_schema NOT IN ('pg_catalog', 'information_schema') \
                   AND c.table_schema NOT LIKE 'pg_toast%' \
                 ORDER BY c.table_schema, c.table_name, c.ordinal_position",
            )
            .await?;
        let keys = self
            .rows(
                "SELECT kcu.table_schema, kcu.table_name, kcu.column_name, tc.constraint_name \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY'",
            )
            .await?;
        let indexes = self
            .rows(
                "SELECT ns.nspname, t.relname, i.relname, \
                   CASE WHEN ix.indisunique THEN '1' ELSE '' END, \
                   CASE WHEN ix.indisprimary THEN '1' ELSE '' END, \
                   CASE WHEN 0 = ANY(ix.indkey) THEN '' ELSE \
                     array_to_string(ARRAY(SELECT a.attname FROM unnest(ix.indkey) WITH ORDINALITY k(n, o) \
                       JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.n ORDER BY k.o), chr(31)) END \
                 FROM pg_index ix \
                 JOIN pg_class i ON i.oid = ix.indexrelid JOIN pg_class t ON t.oid = ix.indrelid \
                 JOIN pg_namespace ns ON ns.oid = t.relnamespace \
                 WHERE ns.nspname NOT IN ('pg_catalog', 'information_schema') \
                   AND ns.nspname NOT LIKE 'pg_toast%' \
                 ORDER BY i.relname",
            )
            .await?;
        // Column lists in key order, joined with the unit separator so names
        // with commas survive.
        let foreign_keys = self
            .rows(
                "SELECT con.conname, ns.nspname, cl.relname, \
                   array_to_string(ARRAY(SELECT a.attname FROM unnest(con.conkey) WITH ORDINALITY k(n, i) \
                     JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n ORDER BY k.i), chr(31)), \
                   fns.nspname, fcl.relname, \
                   array_to_string(ARRAY(SELECT a.attname FROM unnest(con.confkey) WITH ORDINALITY k(n, i) \
                     JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.n ORDER BY k.i), chr(31)), \
                   CASE con.confdeltype WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL' \
                     WHEN 'd' THEN 'SET DEFAULT' WHEN 'r' THEN 'RESTRICT' ELSE '' END \
                 FROM pg_constraint con \
                 JOIN pg_class cl ON cl.oid = con.conrelid JOIN pg_namespace ns ON ns.oid = cl.relnamespace \
                 JOIN pg_class fcl ON fcl.oid = con.confrelid JOIN pg_namespace fns ON fns.oid = fcl.relnamespace \
                 WHERE con.contype = 'f' AND ns.nspname NOT IN ('pg_catalog', 'information_schema') \
                 ORDER BY con.conname",
            )
            .await?;
        Ok(build_schema(columns, keys, indexes, foreign_keys))
    }

    async fn rows(&self, query: &str) -> Result<Vec<Vec<String>>> {
        let messages = self.client.simple_query(query).await.map_err(error)?;
        Ok(messages
            .into_iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::Row(row) => Some(
                    (0..row.len())
                        .map(|i| row.get(i).unwrap_or_default().to_string())
                        .collect(),
                ),
                _ => None,
            })
            .collect())
    }
}

/// Builds objects from `(namespace, table, kind, column, type, nullable,
/// default, auto)` rows, primary keys `(namespace, table, column,
/// constraint)`, indexes `(namespace, table, name, unique, primary,
/// columns)` and foreign keys
/// `(name, namespace, table, columns, ref namespace, ref table, ref columns)`
/// with column lists separated by U+001F. Shared with MySQL, whose
/// information schema has the same shape.
pub(crate) fn build_schema(
    columns: Vec<Vec<String>>,
    keys: Vec<Vec<String>>,
    indexes: Vec<Vec<String>>,
    foreign_keys: Vec<Vec<String>>,
) -> Schema {
    let key = |r: &[String]| (r[0].clone(), r[1].clone());
    let mut primary: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut primary_names: HashMap<(String, String), String> = HashMap::new();
    for row in keys {
        primary.entry(key(&row)).or_default().push(row[2].clone());
        if let Some(name) = row.get(3) {
            primary_names.insert(key(&row), name.clone());
        }
    }
    let split = |s: &str| -> Vec<String> {
        s.split('\u{1f}')
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mut table_indexes: HashMap<(String, String), Vec<Index>> = HashMap::new();
    for row in indexes {
        table_indexes.entry(key(&row)).or_default().push(Index {
            name: row[2].clone(),
            unique: row[3] == "1",
            primary: row[4] == "1",
            columns: split(&row[5]),
        });
    }
    let mut references: HashMap<(String, String), Vec<ForeignKey>> = HashMap::new();
    for row in foreign_keys {
        references
            .entry((row[1].clone(), row[2].clone()))
            .or_default()
            .push(ForeignKey {
                name: row[0].clone(),
                columns: split(&row[3]),
                ref_namespace: Some(row[4].clone()),
                ref_table: row[5].clone(),
                ref_columns: split(&row[6]),
                on_delete: row
                    .get(7)
                    .filter(|r| !r.is_empty() && *r != "NO ACTION")
                    .cloned(),
            });
    }
    let mut objects: Vec<Object> = Vec::new();
    for row in columns {
        let id = key(&row);
        if objects
            .last()
            .is_none_or(|o| o.namespace.as_deref() != Some(&id.0) || o.name != id.1)
        {
            objects.push(Object {
                namespace: Some(id.0.clone()),
                name: id.1.clone(),
                kind: if row[2].contains("VIEW") {
                    ObjectKind::View
                } else {
                    ObjectKind::Table
                },
                columns: Vec::new(),
                indexes: table_indexes.remove(&id).unwrap_or_default(),
                foreign_keys: references.remove(&id).unwrap_or_default(),
                primary_key_name: primary_names.remove(&id),
            });
        }
        let pk = primary.get(&id).is_some_and(|cols| cols.contains(&row[3]));
        if let Some(object) = objects.last_mut() {
            object.columns.push(ColumnInfo {
                name: row[3].clone(),
                type_name: row[4].clone(),
                nullable: row[5] == "YES",
                primary_key: pk,
                default: row.get(6).filter(|d| !d.is_empty()).cloned(),
                auto: row.get(7).is_some_and(|a| a == "1"),
            });
        }
    }
    Schema {
        objects,
        truncated: false,
    }
}

fn decode(text: &str, ty: Option<&Type>) -> Value {
    let Some(ty) = ty else {
        return Value::Text(text.to_string());
    };
    match *ty {
        Type::BOOL => Value::Bool(text == "t"),
        Type::INT2 | Type::INT4 | Type::INT8 | Type::OID => text
            .parse()
            .map_or_else(|_| Value::Number(text.into()), Value::Int),
        Type::FLOAT4 | Type::FLOAT8 => text
            .parse()
            .map_or_else(|_| Value::Number(text.into()), Value::Float),
        Type::NUMERIC => Value::Number(text.into()),
        Type::JSON | Type::JSONB => Value::Json(text.into()),
        Type::BYTEA => text
            .strip_prefix("\\x")
            .and_then(hex)
            .map_or_else(|| Value::Text(text.into()), Value::Bytes),
        _ => Value::Text(text.into()),
    }
}

fn hex(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn error(e: tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => match db.detail() {
            Some(detail) => format!("{}: {}", db.message(), detail),
            None => db.message().to_string(),
        },
        None => {
            let mut message = e.to_string();
            let mut source = std::error::Error::source(&e);
            while let Some(s) = source {
                message = format!("{message}: {s}");
                source = s.source();
            }
            message
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_text_by_type() {
        assert_eq!(decode("t", Some(&Type::BOOL)), Value::Bool(true));
        assert_eq!(decode("42", Some(&Type::INT8)), Value::Int(42));
        assert_eq!(decode("1.5", Some(&Type::FLOAT8)), Value::Float(1.5));
        assert_eq!(
            decode("12.30", Some(&Type::NUMERIC)),
            Value::Number("12.30".into())
        );
        assert_eq!(
            decode("\\x0aff", Some(&Type::BYTEA)),
            Value::Bytes(vec![10, 255])
        );
        assert_eq!(
            decode("{\"a\":1}", Some(&Type::JSONB)),
            Value::Json("{\"a\":1}".into())
        );
        assert_eq!(decode("2024-01-01", None), Value::Text("2024-01-01".into()));
    }
}
