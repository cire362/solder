use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags, types::ValueRef};

use crate::{
    Column, ColumnInfo, ConnectionSpec, Object, ObjectKind, QueryResult, Result, Schema, Value,
};

/// rusqlite is blocking: every call runs on Tokio's blocking pool.
pub struct Sqlite {
    conn: Arc<Mutex<Connection>>,
}

impl Sqlite {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        let path = spec.url.clone();
        // Never create a database file that is not there.
        let flags = if spec.read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let conn = blocking(move || {
            Connection::open_with_flags(&path, flags).map_err(|e| format!("{path}: {e}"))
        })
        .await?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn query(&self, text: String) -> Result<QueryResult> {
        let conn = self.conn.clone();
        blocking(move || run(&conn.lock().unwrap(), &text)).await
    }

    pub async fn schema(&self) -> Result<Schema> {
        let conn = self.conn.clone();
        blocking(move || read_schema(&conn.lock().unwrap())).await
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

fn run(conn: &Connection, text: &str) -> Result<QueryResult> {
    let mut stmt = conn.prepare(text).map_err(|e| e.to_string())?;
    let mut result = QueryResult {
        columns: stmt
            .columns()
            .iter()
            .map(|c| Column {
                name: c.name().to_string(),
                type_name: c.decl_type().unwrap_or_default().to_ascii_lowercase(),
            })
            .collect(),
        ..Default::default()
    };
    if result.columns.is_empty() {
        let changed = stmt.execute([]).map_err(|e| e.to_string())?;
        result.affected = Some(changed as u64);
        return Ok(result);
    }
    let count = result.columns.len();
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let values = (0..count)
            .map(|i| match row.get_ref(i) {
                Ok(ValueRef::Null) | Err(_) => Value::Null,
                Ok(ValueRef::Integer(i)) => Value::Int(i),
                Ok(ValueRef::Real(f)) => Value::Float(f),
                Ok(ValueRef::Text(t)) => Value::Text(String::from_utf8_lossy(t).into_owned()),
                Ok(ValueRef::Blob(b)) => Value::Bytes(b.to_vec()),
            })
            .collect();
        if !result.push_row(values) {
            break;
        }
    }
    Ok(result)
}

fn read_schema(conn: &Connection) -> Result<Schema> {
    let err = |e: rusqlite::Error| e.to_string();
    let mut stmt = conn
        .prepare(
            "SELECT name, type FROM sqlite_schema \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(err)?;
    let tables: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(err)?
        .collect::<std::result::Result<_, _>>()
        .map_err(err)?;
    let mut objects = Vec::new();
    for (name, kind) in tables {
        let mut info = conn
            .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1)")
            .map_err(err)?;
        let columns = info
            .query_map([&name], |r| {
                Ok(ColumnInfo {
                    name: r.get(0)?,
                    type_name: r.get::<_, String>(1)?.to_ascii_lowercase(),
                    nullable: r.get::<_, i64>(2)? == 0,
                    primary_key: r.get::<_, i64>(3)? > 0,
                })
            })
            .map_err(err)?
            .collect::<std::result::Result<_, _>>()
            .map_err(err)?;
        let mut list = conn
            .prepare("SELECT name FROM pragma_index_list(?1) ORDER BY name")
            .map_err(err)?;
        let indexes = list
            .query_map([&name], |r| r.get(0))
            .map_err(err)?
            .collect::<std::result::Result<_, _>>()
            .map_err(err)?;
        objects.push(Object {
            namespace: None,
            name,
            kind: if kind == "view" {
                ObjectKind::View
            } else {
                ObjectKind::Table
            },
            columns,
            indexes,
        });
    }
    Ok(Schema {
        objects,
        truncated: false,
    })
}

#[cfg(test)]
mod tests {
    use crate::{ConnectionSpec, Engine, Session, Value};

    fn database(name: &str, read_only: bool) -> ConnectionSpec {
        let path = std::env::temp_dir().join(format!("solder-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, avatar BLOB);
             CREATE INDEX users_name ON users(name);
             CREATE VIEW names AS SELECT name FROM users;
             INSERT INTO users (name, score, avatar) VALUES ('ada', 9.5, x'00ff'), ('bob', NULL, NULL);",
        )
        .unwrap();
        ConnectionSpec {
            name: name.into(),
            engine: Engine::Sqlite,
            url: path.display().to_string(),
            source: "test".into(),
            read_only,
        }
    }

    fn block<T>(f: impl std::future::Future<Output = T>) -> T {
        futures::executor::block_on(f)
    }

    #[test]
    fn queries_schema_and_writes() {
        let session = block(Session::connect(database("sqlite-rw", false))).unwrap();
        let result =
            block(session.query("SELECT id, name, score, avatar FROM users ORDER BY id".into()))
                .unwrap();
        assert_eq!(
            result
                .columns
                .iter()
                .map(|c| c.type_name.as_str())
                .collect::<Vec<_>>(),
            ["integer", "text", "real", "blob"]
        );
        assert_eq!(
            result.rows[0],
            vec![
                Value::Int(1),
                Value::Text("ada".into()),
                Value::Float(9.5),
                Value::Bytes(vec![0, 255])
            ]
        );
        assert_eq!(result.rows[1][2], Value::Null);

        let update = block(session.query("UPDATE users SET score = 1".into())).unwrap();
        assert_eq!(update.affected, Some(2));
        assert!(
            block(session.query("SELEC nope".into()))
                .unwrap_err()
                .contains("syntax error")
        );

        let schema = block(session.schema()).unwrap();
        let users = schema.objects.iter().find(|o| o.name == "users").unwrap();
        assert!(users.columns[0].primary_key && !users.columns[1].nullable);
        assert_eq!(users.indexes, ["users_name"]);
        assert!(
            schema
                .objects
                .iter()
                .any(|o| o.name == "names" && o.kind == crate::ObjectKind::View)
        );
        assert_eq!(
            session.preview_query(users),
            format!("SELECT * FROM \"users\" LIMIT {}", crate::PREVIEW_ROWS)
        );
    }

    #[test]
    fn read_only_refuses_writes_and_missing_files_are_not_created() {
        let session = block(Session::connect(database("sqlite-ro", true))).unwrap();
        let err = block(session.query("DELETE FROM users".into())).unwrap_err();
        assert!(err.contains("readonly"), "{err}");

        let mut missing = database("sqlite-missing", false);
        std::fs::remove_file(&missing.url).unwrap();
        missing.url.push_str(".nope");
        assert!(block(Session::connect(missing.clone())).is_err());
        assert!(!std::path::Path::new(&missing.url).exists());
    }
}
