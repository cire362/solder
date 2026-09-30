use std::collections::HashMap;

use tokio_postgres::{Client, SimpleQueryMessage, types::Type};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::{
    Column, ColumnInfo, ConnectionSpec, Object, ObjectKind, QueryResult, Result, Schema, Value,
    params::{self, Tls},
    tls,
};

pub struct Pg {
    client: Client,
    prepare: bool,
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
        })
    }

    /// The simple protocol runs any statement and returns text; preparing
    /// the statement first (without running it) tells the column types, so
    /// numbers, booleans and JSON are shown as such.
    pub async fn query(&self, text: &str) -> Result<QueryResult> {
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

    pub async fn schema(&self) -> Result<Schema> {
        let columns = self
            .rows(
                "SELECT c.table_schema, c.table_name, t.table_type, c.column_name, c.data_type, c.is_nullable \
                 FROM information_schema.columns c \
                 JOIN information_schema.tables t USING (table_schema, table_name) \
                 WHERE c.table_schema NOT IN ('pg_catalog', 'information_schema') \
                   AND c.table_schema NOT LIKE 'pg_toast%' \
                 ORDER BY c.table_schema, c.table_name, c.ordinal_position",
            )
            .await?;
        let keys = self
            .rows(
                "SELECT kcu.table_schema, kcu.table_name, kcu.column_name \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY'",
            )
            .await?;
        let indexes = self
            .rows(
                "SELECT schemaname, tablename, indexname FROM pg_indexes \
                 WHERE schemaname NOT IN ('pg_catalog', 'information_schema') ORDER BY indexname",
            )
            .await?;
        Ok(build_schema(columns, keys, indexes))
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

/// Builds objects from `(namespace, table, kind, column, type, nullable)`
/// rows plus primary keys and index names. Shared with MySQL, whose
/// information schema has the same shape.
pub(crate) fn build_schema(
    columns: Vec<Vec<String>>,
    keys: Vec<Vec<String>>,
    indexes: Vec<Vec<String>>,
) -> Schema {
    let key = |r: &[String]| (r[0].clone(), r[1].clone());
    let mut primary: HashMap<(String, String), Vec<String>> = HashMap::new();
    for row in keys {
        primary.entry(key(&row)).or_default().push(row[2].clone());
    }
    let mut index_names: HashMap<(String, String), Vec<String>> = HashMap::new();
    for row in indexes {
        index_names
            .entry(key(&row))
            .or_default()
            .push(row[2].clone());
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
                indexes: index_names.remove(&id).unwrap_or_default(),
            });
        }
        let pk = primary.get(&id).is_some_and(|cols| cols.contains(&row[3]));
        if let Some(object) = objects.last_mut() {
            object.columns.push(ColumnInfo {
                name: row[3].clone(),
                type_name: row[4].clone(),
                nullable: row[5] == "YES",
                primary_key: pk,
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
