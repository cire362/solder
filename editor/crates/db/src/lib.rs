//! Database connections for the Database tab: finding them in a project,
//! connecting, running one statement at a time and reading the schema.
//!
//! Drivers are async and run on a small Tokio runtime that starts with the
//! first connection, so opening the editor never pays for it. Every public
//! future is spawned onto that runtime and can be awaited from any executor.

pub mod complete;
mod detect;
pub mod edit;
mod mongo;
mod mysql;
mod params;
mod pg;
mod redis;
pub mod sql;
mod sqlite;
#[doc(hidden)]
pub mod testing;
mod tls;

use std::{
    future::Future,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

pub use detect::{CUSTOM_FILE, detect, parse_custom, parse_env};

pub type Result<T> = std::result::Result<T, String>;

/// Rows kept from one result. The grid scrolls lazily, but the rows are held
/// in memory, so an accidental `SELECT *` on a large table stops here.
pub const ROW_LIMIT: usize = 10_000;
/// Rows a table preview asks for.
pub const PREVIEW_ROWS: usize = 200;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Engine {
    Postgres,
    MySql,
    Sqlite,
    Redis,
    Mongo,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Engine::Postgres => "postgres",
            Engine::MySql => "mysql",
            Engine::Sqlite => "sqlite",
            Engine::Redis => "redis",
            Engine::Mongo => "mongodb",
        }
    }

    /// The engine a connection URL is for.
    pub fn from_url(url: &str) -> Option<Engine> {
        let scheme = url.split_once(':')?.0.to_ascii_lowercase();
        Some(match scheme.as_str() {
            "postgres" | "postgresql" => Engine::Postgres,
            "mysql" | "mariadb" => Engine::MySql,
            "sqlite" | "sqlite3" | "file" => Engine::Sqlite,
            "redis" | "rediss" => Engine::Redis,
            "mongodb" | "mongodb+srv" => Engine::Mongo,
            _ => return None,
        })
    }

    pub fn is_sql(self) -> bool {
        matches!(self, Engine::Postgres | Engine::MySql | Engine::Sqlite)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionSpec {
    /// Unique within the project.
    pub name: String,
    pub engine: Engine,
    /// Connection URL, or an absolute file path for SQLite. May contain a
    /// password: show `display_url` instead.
    pub url: String,
    /// Where it was found, relative to the project root (".env",
    /// "docker-compose.yml").
    pub source: String,
    /// Production connections refuse writes until unlocked.
    pub read_only: bool,
}

impl ConnectionSpec {
    /// The URL with its password replaced.
    pub fn display_url(&self) -> String {
        match url::Url::parse(&self.url) {
            Ok(mut url) if url.password().is_some() => {
                let _ = url.set_password(Some("•••"));
                url.to_string()
            }
            _ => self.url.clone(),
        }
    }
}

/// A readable default name for a URL: `app on localhost`, or the file name.
pub fn suggested_name(url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(url)
        && let Some(host) = parsed.host_str()
    {
        let db = parsed.path().trim_matches('/');
        return if db.is_empty() {
            host.to_string()
        } else {
            format!("{db} on {host}")
        };
    }
    std::path::Path::new(url)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| url.to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Exact numbers that do not fit `Int` or `Float` (decimals, u64).
    Number(String),
    Text(String),
    Bytes(Vec<u8>),
    /// JSON columns and nested documents, as JSON text.
    Json(String),
}

impl Value {
    pub fn is_numeric(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Float(_) | Value::Number(_))
    }

    /// One line of text for a grid cell.
    pub fn display(&self) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Number(s) | Value::Json(s) => s.clone(),
            Value::Text(s) => s.clone(),
            Value::Bytes(b) => {
                let hex: String = b.iter().take(32).map(|b| format!("{b:02x}")).collect();
                if b.len() > 32 {
                    format!("0x{hex}… ({} bytes)", b.len())
                } else {
                    format!("0x{hex}")
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub type_name: String,
}

#[derive(Clone, Debug, Default)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    /// Rows changed by a write, when the server reports it.
    pub affected: Option<u64>,
    /// More rows existed than `ROW_LIMIT`.
    pub truncated: bool,
    pub elapsed: Duration,
}

impl QueryResult {
    fn push_row(&mut self, row: Vec<Value>) -> bool {
        if self.rows.len() >= ROW_LIMIT {
            self.truncated = true;
            return false;
        }
        self.rows.push(row);
        true
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ObjectKind {
    #[default]
    Table,
    View,
    Collection,
    /// A Redis key and its type.
    Key(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub primary_key: bool,
    /// The default expression as the server shows it (`now()`, `'draft'`).
    pub default: Option<String>,
    /// Filled in by the server: serial, identity, auto_increment, SQLite's
    /// rowid alias.
    pub auto: bool,
}

/// A foreign key: `columns` of this table point at `ref_columns` of
/// `ref_table`, in the same order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_namespace: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Object {
    /// Postgres schema, MySQL or Mongo database. `None` where there is one.
    pub namespace: Option<String>,
    pub name: String,
    pub kind: ObjectKind,
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<String>,
    pub foreign_keys: Vec<ForeignKey>,
}

#[derive(Clone, Debug, Default)]
pub struct Schema {
    pub objects: Vec<Object>,
    /// The listing stopped early (Redis with many keys).
    pub truncated: bool,
}

enum Driver {
    Postgres(pg::Pg),
    MySql(mysql::MySql),
    Sqlite(sqlite::Sqlite),
    Redis(redis::Redis),
    Mongo(mongo::Mongo),
}

/// An open connection. Cheap to clone; all clones share it.
#[derive(Clone)]
pub struct Session {
    driver: Arc<Driver>,
    pub spec: Arc<ConnectionSpec>,
}

impl Session {
    pub fn connect(spec: ConnectionSpec) -> impl Future<Output = Result<Session>> + Send + 'static {
        spawn(async move {
            let connect = async {
                Ok::<_, String>(match spec.engine {
                    Engine::Postgres => Driver::Postgres(pg::Pg::connect(&spec).await?),
                    Engine::MySql => Driver::MySql(mysql::MySql::connect(&spec).await?),
                    Engine::Sqlite => Driver::Sqlite(sqlite::Sqlite::connect(&spec).await?),
                    Engine::Redis => Driver::Redis(redis::Redis::connect(&spec).await?),
                    Engine::Mongo => Driver::Mongo(mongo::Mongo::connect(&spec).await?),
                })
            };
            let driver = tokio::time::timeout(CONNECT_TIMEOUT, connect)
                .await
                .map_err(|_| "Timed out connecting".to_string())??;
            Ok(Session {
                driver: Arc::new(driver),
                spec: Arc::new(spec),
            })
        })
    }

    pub fn engine(&self) -> Engine {
        self.spec.engine
    }

    /// Runs one statement (SQL), command line (Redis) or shell call (Mongo).
    pub fn query(
        &self,
        text: String,
    ) -> impl Future<Output = Result<QueryResult>> + Send + 'static {
        let driver = self.driver.clone();
        spawn(async move {
            let start = Instant::now();
            let mut result = match &*driver {
                Driver::Postgres(d) => d.query(&text).await,
                Driver::MySql(d) => d.query(&text).await,
                Driver::Sqlite(d) => d.query(text).await,
                Driver::Redis(d) => d.query(&text).await,
                Driver::Mongo(d) => d.query(&text).await,
            }?;
            result.elapsed = start.elapsed();
            Ok(result)
        })
    }

    pub fn schema(&self) -> impl Future<Output = Result<Schema>> + Send + 'static {
        let driver = self.driver.clone();
        spawn(async move {
            match &*driver {
                Driver::Postgres(d) => d.schema().await,
                Driver::MySql(d) => d.schema().await,
                Driver::Sqlite(d) => d.schema().await,
                Driver::Redis(d) => d.schema().await,
                Driver::Mongo(d) => d.schema().await,
            }
        })
    }

    /// Runs `statements` in one transaction. Each must change exactly one
    /// row; otherwise, or on any error, everything is rolled back.
    pub fn apply(
        &self,
        statements: Vec<String>,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let driver = self.driver.clone();
        spawn(async move {
            match &*driver {
                Driver::Postgres(d) => d.apply(&statements).await,
                Driver::MySql(d) => d.apply(&statements).await,
                Driver::Sqlite(d) => d.apply(statements).await,
                Driver::Redis(_) | Driver::Mongo(_) => {
                    Err("Editing works for Postgres, MySQL and SQLite results".into())
                }
            }
        })
    }

    /// The query that shows the first rows of `object`.
    pub fn preview_query(&self, object: &Object) -> String {
        preview_query(self.engine(), object)
    }
}

pub fn preview_query(engine: Engine, object: &Object) -> String {
    match engine {
        Engine::Postgres | Engine::MySql | Engine::Sqlite => format!(
            "SELECT * FROM {} LIMIT {PREVIEW_ROWS}",
            sql::qualified_name(engine, object.namespace.as_deref(), &object.name)
        ),
        Engine::Redis => {
            let key = redis::quote_arg(&object.name);
            match &object.kind {
                ObjectKind::Key(t) if t == "hash" => format!("HGETALL {key}"),
                ObjectKind::Key(t) if t == "list" => format!("LRANGE {key} 0 {}", PREVIEW_ROWS - 1),
                ObjectKind::Key(t) if t == "set" => format!("SMEMBERS {key}"),
                ObjectKind::Key(t) if t == "zset" => {
                    format!("ZRANGE {key} 0 {} WITHSCORES", PREVIEW_ROWS - 1)
                }
                ObjectKind::Key(t) if t == "stream" => {
                    format!("XRANGE {key} - + COUNT {PREVIEW_ROWS}")
                }
                _ => format!("GET {key}"),
            }
        }
        Engine::Mongo => mongo::preview_query(object),
    }
}

/// Why a staged change could not be saved: statement `index` (from 0) of
/// `total` matched `affected` rows instead of one.
pub(crate) fn unmatched(index: usize, total: usize, affected: u64) -> String {
    let what = if affected == 0 {
        "matched no row (it changed or was deleted since it was read)".to_string()
    } else {
        format!("matched {affected} rows")
    };
    format!("Change {} of {total} {what}. Nothing was saved.", index + 1)
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        // Several drivers build rustls configs from the process default.
        let _ = rustls::crypto::ring::default_provider().install_default();
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("solder-db")
            .enable_all()
            .build()
            .expect("database runtime")
    })
}

/// Runs `future` on the database runtime; the returned future can be awaited
/// from GPUI's executors.
pub fn spawn<T: Send + 'static>(
    future: impl Future<Output = Result<T>> + Send + 'static,
) -> impl Future<Output = Result<T>> + Send + 'static {
    let handle = runtime().spawn(future);
    async move { handle.await.map_err(|e| e.to_string())? }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_names() {
        assert_eq!(
            suggested_name("postgres://u:p@db.local:5432/app"),
            "app on db.local"
        );
        assert_eq!(suggested_name("redis://localhost:6379"), "localhost");
        assert_eq!(suggested_name("/tmp/data/dev.db"), "dev.db");
        assert_eq!(Engine::from_url("mongodb+srv://x"), Some(Engine::Mongo));
        assert_eq!(Engine::from_url("https://x"), None);
    }
}
