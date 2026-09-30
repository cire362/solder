use mysql_async::{
    Conn, Opts, OptsBuilder, Row, SslOpts,
    consts::{ColumnFlags, ColumnType},
    prelude::Queryable,
};
use tokio::sync::Mutex;

use crate::{
    Column, ConnectionSpec, QueryResult, Result, Schema, Value,
    params::{self, Tls},
    pg::build_schema,
};

pub struct MySql {
    conn: Mutex<Conn>,
}

impl MySql {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        let params = params::mysql(&spec.url)?;
        let opts = Opts::from_url(&params.url).map_err(|e| e.to_string())?;
        let ssl = match params.tls {
            Tls::Off => None,
            Tls::Unverified => Some(SslOpts::default().with_danger_accept_invalid_certs(true)),
            Tls::Verified(None) => Some(SslOpts::default()),
            Tls::Verified(Some(root)) => Some(
                SslOpts::default()
                    .with_root_certs(vec![root.into()])
                    .with_disable_built_in_roots(true),
            ),
        };
        // Report matched rows, not changed ones, so saving an unchanged value
        // still counts as the one row it targets.
        let opts = OptsBuilder::from_opts(opts)
            .ssl_opts(ssl)
            .client_found_rows(true);
        let mut conn = Conn::new(opts).await.map_err(|e| e.to_string())?;
        if spec.read_only {
            conn.query_drop("SET SESSION TRANSACTION READ ONLY")
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub async fn query(&self, text: &str) -> Result<QueryResult> {
        let mut conn = self.conn.lock().await;
        let mut rows = conn.query_iter(text).await.map_err(|e| e.to_string())?;
        let mut result = QueryResult::default();
        let columns = rows.columns().unwrap_or_default();
        result.columns = columns
            .iter()
            .map(|c| Column {
                name: c.name_str().into_owned(),
                type_name: type_name(c.column_type()).to_string(),
            })
            .collect();
        while let Some(row) = rows.next().await.map_err(|e| e.to_string())? {
            // Past the limit rows are still read so the connection stays usable.
            result.push_row(decode_row(row, &columns));
        }
        if result.columns.is_empty() {
            result.affected = Some(rows.affected_rows());
        }
        // Later result sets (a multi-statement selection) are dropped.
        drop(rows);
        Ok(result)
    }

    pub async fn apply(&self, statements: &[String]) -> Result<()> {
        let mut conn = self.conn.lock().await;
        conn.query_drop("START TRANSACTION")
            .await
            .map_err(|e| e.to_string())?;
        for (i, statement) in statements.iter().enumerate() {
            let affected = match conn.query_iter(statement.as_str()).await {
                Ok(result) => {
                    let affected = result.affected_rows();
                    result.drop_result().await.map_err(|e| e.to_string())?;
                    affected
                }
                Err(e) => {
                    let _ = conn.query_drop("ROLLBACK").await;
                    return Err(format!("{e}. Nothing was saved."));
                }
            };
            if affected != 1 {
                let _ = conn.query_drop("ROLLBACK").await;
                return Err(crate::unmatched(i, statements.len(), affected));
            }
        }
        conn.query_drop("COMMIT").await.map_err(|e| e.to_string())
    }

    pub async fn schema(&self) -> Result<Schema> {
        let mut conn = self.conn.lock().await;
        // Without a database in the URL, every non-system schema.
        let filter = "c.TABLE_SCHEMA = COALESCE(DATABASE(), c.TABLE_SCHEMA) \
             AND c.TABLE_SCHEMA NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')";
        let columns: Vec<Row> = conn
            .query(format!(
                "SELECT c.TABLE_SCHEMA, c.TABLE_NAME, t.TABLE_TYPE, c.COLUMN_NAME, c.COLUMN_TYPE, c.IS_NULLABLE \
                 FROM information_schema.COLUMNS c \
                 JOIN information_schema.TABLES t ON t.TABLE_SCHEMA = c.TABLE_SCHEMA AND t.TABLE_NAME = c.TABLE_NAME \
                 WHERE {filter} ORDER BY c.TABLE_SCHEMA, c.TABLE_NAME, c.ORDINAL_POSITION"
            ))
            .await
            .map_err(|e| e.to_string())?;
        let keys: Vec<Row> = conn
            .query(format!(
                "SELECT c.TABLE_SCHEMA, c.TABLE_NAME, c.COLUMN_NAME FROM information_schema.COLUMNS c \
                 WHERE {filter} AND c.COLUMN_KEY = 'PRI'"
            ))
            .await
            .map_err(|e| e.to_string())?;
        let indexes: Vec<Row> = conn
            .query(format!(
                "SELECT DISTINCT c.TABLE_SCHEMA, c.TABLE_NAME, c.INDEX_NAME FROM information_schema.STATISTICS c \
                 WHERE {filter} ORDER BY c.INDEX_NAME"
            ))
            .await
            .map_err(|e| e.to_string())?;
        Ok(build_schema(
            strings(columns),
            strings(keys),
            strings(indexes),
        ))
    }
}

fn strings(rows: Vec<Row>) -> Vec<Vec<String>> {
    rows.into_iter()
        .map(|row| {
            row.unwrap()
                .into_iter()
                .map(|v| match v {
                    mysql_async::Value::Bytes(b) => String::from_utf8_lossy(&b).into_owned(),
                    mysql_async::Value::NULL => String::new(),
                    other => other.as_sql(true),
                })
                .collect()
        })
        .collect()
}

fn decode_row(row: Row, columns: &[mysql_async::Column]) -> Vec<Value> {
    row.unwrap()
        .into_iter()
        .zip(columns)
        .map(|(value, column)| decode(value, column))
        .collect()
}

/// The text protocol sends every value as bytes; the column type says what
/// they are.
fn decode(value: mysql_async::Value, column: &mysql_async::Column) -> Value {
    use ColumnType::*;
    let bytes = match value {
        mysql_async::Value::NULL => return Value::Null,
        mysql_async::Value::Bytes(b) => b,
        mysql_async::Value::Int(i) => return Value::Int(i),
        mysql_async::Value::UInt(u) => {
            return i64::try_from(u).map_or_else(|_| Value::Number(u.to_string()), Value::Int);
        }
        mysql_async::Value::Float(f) => return Value::Float(f as f64),
        mysql_async::Value::Double(f) => return Value::Float(f),
        other => return Value::Text(other.as_sql(true).trim_matches('\'').to_string()),
    };
    let binary = column.character_set() == 63;
    match column.column_type() {
        MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG
        | MYSQL_TYPE_INT24 | MYSQL_TYPE_YEAR => {
            let text = String::from_utf8_lossy(&bytes);
            text.parse()
                .map_or_else(|_| Value::Number(text.into_owned()), Value::Int)
        }
        MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => {
            let text = String::from_utf8_lossy(&bytes);
            text.parse()
                .map_or_else(|_| Value::Number(text.into_owned()), Value::Float)
        }
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => {
            Value::Number(String::from_utf8_lossy(&bytes).into_owned())
        }
        MYSQL_TYPE_JSON => Value::Json(String::from_utf8_lossy(&bytes).into_owned()),
        MYSQL_TYPE_BIT | MYSQL_TYPE_GEOMETRY => Value::Bytes(bytes),
        MYSQL_TYPE_TINY_BLOB
        | MYSQL_TYPE_MEDIUM_BLOB
        | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_BLOB
        | MYSQL_TYPE_STRING
        | MYSQL_TYPE_VAR_STRING
            if binary && !column.flags().contains(ColumnFlags::ENUM_FLAG) =>
        {
            Value::Bytes(bytes)
        }
        _ => match String::from_utf8(bytes) {
            Ok(text) => Value::Text(text),
            Err(e) => Value::Bytes(e.into_bytes()),
        },
    }
}

fn type_name(t: ColumnType) -> &'static str {
    use ColumnType::*;
    match t {
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_BIT => "bit",
        MYSQL_TYPE_ENUM => "enum",
        MYSQL_TYPE_SET => "set",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            "blob"
        }
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => "varchar",
        MYSQL_TYPE_STRING => "char",
        MYSQL_TYPE_GEOMETRY => "geometry",
        _ => "",
    }
}
