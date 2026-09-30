//! The project's database connections, shared by the Database tab and the
//! results grid. Detection runs the first time either needs it, and a
//! connection opens the first time something runs on it, so projects that
//! never touch a database pay nothing.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use db::{ConnectionSpec, Engine, QueryResult, Schema, Session};
use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use gpui::{AppContext, Context, EventEmitter, SharedString, Task};

type Connecting = Shared<BoxFuture<'static, Result<Session, String>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Connecting,
    Ready,
    Failed(SharedString),
}

#[derive(Clone)]
pub enum SchemaState {
    NotLoaded,
    Loading,
    Loaded(Arc<Schema>),
    Failed(SharedString),
}

pub struct Connection {
    pub spec: ConnectionSpec,
    pub status: Status,
    pub schema: SchemaState,
    /// A read-only connection the user unlocked for writes until quit.
    pub unlocked: bool,
    session: Option<Connecting>,
}

impl Connection {
    /// Writes are refused: a production connection nobody unlocked.
    pub fn locked(&self) -> bool {
        self.spec.read_only && !self.unlocked
    }
}

pub enum DatabaseEvent {
    /// Detection finished or a connection's status or schema changed.
    Changed,
}

impl EventEmitter<DatabaseEvent> for DatabaseStore {}

pub struct DatabaseStore {
    root: PathBuf,
    connections: Vec<Connection>,
    detected: bool,
    detect_task: Option<Task<()>>,
    /// Which connection each query file runs on.
    bindings: HashMap<PathBuf, String>,
    /// The connection used most recently, the default for new query files.
    last_used: Option<String>,
}

/// Files `cmd-enter` runs, and the engines each kind of file suits.
pub fn query_file_engines(path: &Path) -> Option<&'static [Engine]> {
    const SQL: &[Engine] = &[Engine::Postgres, Engine::MySql, Engine::Sqlite];
    match path.extension()?.to_str()? {
        "sql" | "psql" | "pgsql" | "mysql" => Some(SQL),
        "redis" => Some(&[Engine::Redis]),
        "mongodb" | "mongo" => Some(&[Engine::Mongo]),
        _ => None,
    }
}

/// Connections added with New connection live outside the project.
pub fn user_file() -> PathBuf {
    crate::settings::config_dir().join("connections.json")
}

impl DatabaseStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            connections: Vec::new(),
            detected: false,
            detect_task: None,
            bindings: HashMap::new(),
            last_used: None,
        }
    }

    pub fn binding(&self, path: &Path) -> Option<&str> {
        self.bindings
            .get(path)
            .map(String::as_str)
            .filter(|name| self.connections.iter().any(|c| c.spec.name == *name))
    }

    pub fn bind(&mut self, path: PathBuf, name: String, cx: &mut Context<Self>) {
        self.last_used = Some(name.clone());
        let load = self
            .connections
            .iter()
            .find(|c| c.spec.name == name)
            .is_some_and(|c| matches!(c.schema, SchemaState::NotLoaded));
        self.bindings.insert(path, name.clone());
        if load {
            self.load_schema(&name, cx);
        }
        cx.emit(DatabaseEvent::Changed);
        cx.notify();
    }

    /// The connection a new query file of this kind should use: the last one
    /// used if it suits, or the only one that does.
    pub fn default_for(&self, engines: &[Engine]) -> Option<String> {
        let suits = |c: &&Connection| engines.contains(&c.spec.engine);
        if let Some(last) = &self.last_used
            && self
                .connections
                .iter()
                .filter(suits)
                .any(|c| &c.spec.name == last)
        {
            return Some(last.clone());
        }
        let mut suitable = self.connections.iter().filter(suits);
        match (suitable.next(), suitable.next()) {
            (Some(only), None) => Some(only.spec.name.clone()),
            _ => None,
        }
    }

    pub fn engine_of(&self, name: &str) -> Option<Engine> {
        self.connections
            .iter()
            .find(|c| c.spec.name == name)
            .map(|c| c.spec.engine)
    }

    pub fn schema_of(&self, name: &str) -> Option<(Engine, Arc<Schema>)> {
        let conn = self.connections.iter().find(|c| c.spec.name == name)?;
        match &conn.schema {
            SchemaState::Loaded(schema) => Some((conn.spec.engine, schema.clone())),
            _ => None,
        }
    }

    /// Loads the schema unless it is loaded, loading or failed.
    pub fn ensure_schema(&mut self, name: &str, cx: &mut Context<Self>) {
        if self
            .connections
            .iter()
            .any(|c| c.spec.name == name && matches!(c.schema, SchemaState::NotLoaded))
        {
            self.load_schema(name, cx);
        }
    }

    pub fn set_last_used(&mut self, name: &str) {
        self.last_used = Some(name.to_string());
    }

    pub fn detected(&self) -> bool {
        self.detected
    }

    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    /// Detects once; later calls do nothing until `redetect`.
    pub fn ensure_detected(&mut self, cx: &mut Context<Self>) {
        if !self.detected && self.detect_task.is_none() {
            self.redetect(cx);
        }
    }

    /// Re-reads `.env`, compose and connection files. Open connections whose
    /// spec did not change stay open.
    pub fn redetect(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.detect_task = Some(cx.spawn(async move |this, cx| {
            let specs = cx
                .background_executor()
                .spawn(async move { db::detect(&root, Some(&user_file())) })
                .await;
            this.update(cx, |this, cx| {
                let mut old = std::mem::take(&mut this.connections);
                this.connections = specs
                    .into_iter()
                    .map(|spec| match old.iter().position(|c| c.spec == spec) {
                        Some(ix) => old.swap_remove(ix),
                        None => Connection {
                            spec,
                            status: Status::Idle,
                            schema: SchemaState::NotLoaded,
                            unlocked: false,
                            session: None,
                        },
                    })
                    .collect();
                this.detected = true;
                this.detect_task = None;
                cx.emit(DatabaseEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
    }

    /// The connection's session, opening it if needed. Callers share one
    /// attempt; a failed attempt is retried by the next caller.
    fn session(&mut self, name: &str, cx: &mut Context<Self>) -> Option<Connecting> {
        let conn = self.connections.iter_mut().find(|c| c.spec.name == name)?;
        if let Some(session) = &conn.session {
            return Some(session.clone());
        }
        let spec = ConnectionSpec {
            read_only: conn.locked(),
            ..conn.spec.clone()
        };
        let connecting: Connecting = Session::connect(spec).boxed().shared();
        conn.session = Some(connecting.clone());
        conn.status = Status::Connecting;
        let name = name.to_string();
        let waiter = connecting.clone();
        cx.spawn(async move |this, cx| {
            let result = waiter.await;
            this.update(cx, |this, cx| {
                let Some(conn) = this.connections.iter_mut().find(|c| c.spec.name == name) else {
                    return;
                };
                match result {
                    Ok(_) => conn.status = Status::Ready,
                    Err(e) => {
                        conn.status = Status::Failed(e.into());
                        conn.session = None;
                    }
                }
                cx.emit(DatabaseEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
        Some(connecting)
    }

    pub fn run(
        &mut self,
        name: &str,
        query: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<QueryResult, String>> {
        let Some(session) = self.session(name, cx) else {
            return Task::ready(Err(format!("No connection named {name}")));
        };
        cx.background_spawn(async move { session.await?.query(query).await })
    }

    /// Applies staged edits in one transaction.
    pub fn apply(
        &mut self,
        name: &str,
        statements: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(session) = self.session(name, cx) else {
            return Task::ready(Err(format!("No connection named {name}")));
        };
        cx.background_spawn(async move { session.await?.apply(statements).await })
    }

    pub fn connection(&self, name: &str) -> Option<&Connection> {
        self.connections.iter().find(|c| c.spec.name == name)
    }

    /// Allows writes on a read-only connection until Solder quits. The
    /// session reopens without its read-only setting.
    pub fn unlock(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(conn) = self.connections.iter_mut().find(|c| c.spec.name == name) {
            conn.unlocked = true;
            conn.session = None;
            conn.status = Status::Idle;
            cx.emit(DatabaseEvent::Changed);
            cx.notify();
        }
    }

    pub fn load_schema(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(session) = self.session(name, cx) else {
            return;
        };
        if let Some(conn) = self.connections.iter_mut().find(|c| c.spec.name == name) {
            conn.schema = SchemaState::Loading;
        }
        let name = name.to_string();
        let load = cx.background_spawn(async move {
            let schema: Result<Schema, String> = session.await?.schema().await;
            schema
        });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                if let Some(conn) = this.connections.iter_mut().find(|c| c.spec.name == name) {
                    conn.schema = match result {
                        Ok(schema) => SchemaState::Loaded(Arc::new(schema)),
                        Err(e) => SchemaState::Failed(e.into()),
                    };
                }
                cx.emit(DatabaseEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Closes every session and forgets schemas, then detects again.
    pub fn reconnect_all(&mut self, cx: &mut Context<Self>) {
        for conn in &mut self.connections {
            conn.session = None;
            conn.status = Status::Idle;
            conn.schema = SchemaState::NotLoaded;
        }
        self.redetect(cx);
    }

    /// Saves a connection to the user's file and detects again.
    pub fn add(&mut self, name: String, url: String, cx: &mut Context<Self>) -> Task<()> {
        let root = self.root.clone();
        let path = user_file();
        let task = cx
            .background_executor()
            .spawn(async move { add_to_file(&path, &root, &name, &url) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                eprintln!("could not save the connection: {e}");
            }
            this.update(cx, |this, cx| this.redetect(cx)).ok();
        })
    }
}

fn add_to_file(path: &Path, root: &Path, name: &str, url: &str) -> std::io::Result<()> {
    let mut file: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({ "connections": [] }));
    if !file["connections"].is_array() {
        file["connections"] = serde_json::json!([]);
    }
    if let Some(list) = file["connections"].as_array_mut() {
        list.push(serde_json::json!({ "project": root, "name": name, "url": url }));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&file)? + "\n")?;
    // The file holds passwords.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Files whose change can add or remove connections.
pub fn is_connection_source(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    name.starts_with(".env")
        || name.contains("compose.y")
        || path.ends_with(db::CUSTOM_FILE)
        || [".sqlite", ".sqlite3", ".db"]
            .iter()
            .any(|e| name.ends_with(e))
}

/// Asks before unlocking a read-only connection for this session.
pub fn confirm_unlock(
    store: gpui::Entity<DatabaseStore>,
    name: String,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let answer = window.prompt(
        gpui::PromptLevel::Warning,
        &format!("Allow changes to {name}?"),
        Some("This connection is read-only because it looks like production. Changes stay allowed until Solder quits."),
        &["Allow changes", "Cancel"],
        cx,
    );
    cx.spawn(async move |cx| {
        if answer.await.ok() == Some(0) {
            store.update(cx, |s, cx| s.unlock(&name, cx)).ok();
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_sources() {
        assert!(is_connection_source(Path::new("/p/.env.local")));
        assert!(is_connection_source(Path::new("/p/docker-compose.yml")));
        assert!(is_connection_source(Path::new(
            "/p/.solder/connections.json"
        )));
        assert!(!is_connection_source(Path::new("/p/src/main.rs")));
    }

    #[test]
    fn added_connections_are_per_project_and_private() {
        let dir = std::env::temp_dir().join(format!("solder-conn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("connections.json");
        add_to_file(&file, Path::new("/work/a"), "one", "postgres://localhost/a").unwrap();
        add_to_file(&file, Path::new("/work/b"), "two", "postgres://localhost/b").unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let env = Default::default();
        let a = db::parse_custom(&text, Path::new("/work/a"), "added", &env);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
