//! A Language Server Protocol client over stdio.
//!
//! One reader and one writer thread per server; nothing here blocks the UI.
//! Requests return futures that resolve when the response arrives, and
//! server notifications come out of a channel the app polls on its own terms.
//!
//! A server need not be a process: [`LanguageServer::linked`] is one whose
//! messages go to the app and come from it, for something that speaks the
//! protocol from elsewhere (the code of a VS Code extension, in its host).

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
        mpsc as std_mpsc,
    },
};

use futures::channel::{mpsc, oneshot};
pub use lsp_types as types;
use lsp_types::{
    ClientCapabilities, InitializeParams, InitializedParams, PositionEncodingKind,
    ServerCapabilities, Uri, WorkspaceFolder,
};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub enum Error {
    /// The server process could not be started or its pipes broke.
    Io(String),
    /// The server answered with an error.
    Server { code: i64, message: String },
    /// The server exited before answering.
    Closed,
    /// The response did not have the expected shape.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Server { message, .. } => write!(f, "{message}"),
            Error::Closed => write!(f, "language server exited"),
            Error::Parse(e) => write!(f, "unexpected response: {e}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// How positions count columns. UTF-8 when the server agrees to it, since
/// that is what the editor stores; UTF-16 otherwise, as the protocol requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

/// A message the server sent without being asked. Requests the app must
/// answer (like `workspace/applyEdit`) carry an `id`; reply with
/// [`LanguageServer::respond`].
#[derive(Debug, Clone)]
pub struct Notification {
    pub method: String,
    pub params: Value,
    pub id: Option<Value>,
}

type Pending = Arc<Mutex<HashMap<i32, oneshot::Sender<Result<Value>>>>>;

pub struct LanguageServer {
    name: String,
    next_id: AtomicI32,
    outgoing: std_mpsc::Sender<Vec<u8>>,
    pending: Pending,
    capabilities: Mutex<ServerCapabilities>,
    encoding: Mutex<Encoding>,
    /// What `workspace/configuration` is answered from.
    configuration: Arc<Mutex<Value>>,
    /// The process, for a server that is one.
    child: Mutex<Option<Child>>,
}

/// The other end of a server that is not a process: what it says comes in
/// here, a message at a time. Dropped, the server is gone, and whoever
/// waited for an answer of it learns so.
pub struct Link {
    pending: Pending,
    notifications: mpsc::UnboundedSender<Notification>,
    outgoing: std_mpsc::Sender<Vec<u8>>,
    configuration: Arc<Mutex<Value>>,
}

impl Link {
    pub fn receive(&self, message: Value) {
        dispatch(
            message,
            &self.pending,
            &self.notifications,
            &self.outgoing,
            &self.configuration,
        );
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        for (_, tx) in self.pending.lock().drain() {
            let _ = tx.send(Err(Error::Closed));
        }
    }
}

pub struct ServerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl LanguageServer {
    /// Starts the process. Call [`LanguageServer::initialize`] before anything else.
    pub fn spawn(
        name: impl Into<String>,
        command: &ServerCommand,
        root: &Path,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Notification>)> {
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .envs(command.env.iter().map(|(k, v)| (k, v)))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Io(format!("{}: {e}", command.program.display())))?;
        let stdin = child.stdin.take().ok_or(Error::Closed)?;
        let stdout = child.stdout.take().ok_or(Error::Closed)?;

        let (out_tx, out_rx) = std_mpsc::channel::<Vec<u8>>();
        let (note_tx, note_rx) = mpsc::unbounded();
        let pending: Pending = Arc::default();
        let configuration = Arc::new(Mutex::new(Value::Null));

        std::thread::Builder::new()
            .name("lsp-writer".into())
            .spawn(move || write_loop(stdin, out_rx))
            .map_err(|e| Error::Io(e.to_string()))?;
        {
            let pending = pending.clone();
            let out_tx = out_tx.clone();
            let configuration = configuration.clone();
            std::thread::Builder::new()
                .name("lsp-reader".into())
                .spawn(move || read_loop(stdout, pending, note_tx, out_tx, configuration))
                .map_err(|e| Error::Io(e.to_string()))?;
        }

        let server = Arc::new(Self {
            name: name.into(),
            next_id: AtomicI32::new(1),
            outgoing: out_tx,
            pending,
            capabilities: Mutex::new(ServerCapabilities::default()),
            encoding: Mutex::new(Encoding::Utf16),
            configuration,
            child: Mutex::new(Some(child)),
        });
        Ok((server, note_rx))
    }

    /// A server that is not a process. Every message for it is given to
    /// `send`, on a thread of the server's own, and what it says comes in
    /// through the [`Link`]. Call [`LanguageServer::initialize`] before
    /// anything else, as for any server.
    pub fn linked(
        name: impl Into<String>,
        send: impl Fn(Value) + Send + 'static,
    ) -> Result<(Arc<Self>, Link, mpsc::UnboundedReceiver<Notification>)> {
        let (out_tx, out_rx) = std_mpsc::channel::<Vec<u8>>();
        let (note_tx, note_rx) = mpsc::unbounded();
        let pending: Pending = Arc::default();
        let configuration = Arc::new(Mutex::new(Value::Null));
        std::thread::Builder::new()
            .name("lsp-link".into())
            .spawn(move || {
                for body in out_rx {
                    if let Ok(message) = serde_json::from_slice(&body) {
                        send(message);
                    }
                }
            })
            .map_err(|e| Error::Io(e.to_string()))?;
        let link = Link {
            pending: pending.clone(),
            notifications: note_tx,
            outgoing: out_tx.clone(),
            configuration: configuration.clone(),
        };
        let server = Arc::new(Self {
            name: name.into(),
            next_id: AtomicI32::new(1),
            outgoing: out_tx,
            pending,
            capabilities: Mutex::new(ServerCapabilities::default()),
            encoding: Mutex::new(Encoding::Utf16),
            configuration,
            child: Mutex::new(None),
        });
        Ok((server, link, note_rx))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn capabilities(&self) -> ServerCapabilities {
        self.capabilities.lock().clone()
    }

    pub fn encoding(&self) -> Encoding {
        *self.encoding.lock()
    }

    /// The settings of this server: told to it now, and what its
    /// `workspace/configuration` requests are answered from afterwards.
    pub fn set_configuration(&self, settings: Value) {
        *self.configuration.lock() = settings.clone();
        self.notify::<lsp_types::notification::DidChangeConfiguration>(
            lsp_types::DidChangeConfigurationParams { settings },
        );
    }

    /// The `initialize` handshake. Offers UTF-8 positions first.
    pub async fn initialize(&self, root: &Path, options: Option<Value>) -> Result<()> {
        let root_uri = path_to_uri(root);
        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(root_uri.clone()),
            initialization_options: options,
            capabilities: client_capabilities(),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            }]),
            client_info: Some(lsp_types::ClientInfo {
                name: "Solder".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            ..Default::default()
        };
        let result = self
            .request::<lsp_types::request::Initialize>(params)
            .await?;
        *self.encoding.lock() = match result.capabilities.position_encoding.as_ref() {
            Some(e) if *e == PositionEncodingKind::UTF8 => Encoding::Utf8,
            _ => Encoding::Utf16,
        };
        *self.capabilities.lock() = result.capabilities;
        self.notify::<lsp_types::notification::Initialized>(InitializedParams {});
        Ok(())
    }

    pub fn request<R: lsp_types::request::Request>(
        &self,
        params: R::Params,
    ) -> impl Future<Output = Result<R::Result>> + Send + 'static {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        let sent = self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": R::METHOD,
            "params": params,
        }));
        if sent.is_err() {
            self.pending.lock().remove(&id);
        }
        async move {
            sent?;
            let value = rx.await.map_err(|_| Error::Closed)??;
            serde_json::from_value(value).map_err(|e| Error::Parse(e.to_string()))
        }
    }

    pub fn notify<N: lsp_types::notification::Notification>(&self, params: N::Params) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": N::METHOD,
            "params": params,
        }))
        .ok();
    }

    /// Answers a request the server sent to us.
    pub fn respond(&self, id: Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .ok();
    }

    fn send(&self, message: &impl Serialize) -> Result<()> {
        let body = serde_json::to_vec(message).map_err(|e| Error::Parse(e.to_string()))?;
        self.outgoing.send(body).map_err(|_| Error::Closed)
    }

    /// Polite shutdown, then kill if the server lingers.
    pub async fn shutdown(&self) {
        let _ = self.request::<lsp_types::request::Shutdown>(()).await;
        self.notify::<lsp_types::notification::Exit>(());
    }

    pub fn kill(&self) {
        if let Some(child) = self.child.lock().as_mut() {
            let _ = child.kill();
        }
    }
}

impl Drop for LanguageServer {
    fn drop(&mut self) {
        self.kill();
    }
}

fn write_loop(mut stdin: std::process::ChildStdin, rx: std_mpsc::Receiver<Vec<u8>>) {
    while let Ok(body) = rx.recv() {
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        if stdin.write_all(header.as_bytes()).is_err()
            || stdin.write_all(&body).is_err()
            || stdin.flush().is_err()
        {
            break;
        }
    }
}

fn read_loop(
    stdout: std::process::ChildStdout,
    pending: Pending,
    notifications: mpsc::UnboundedSender<Notification>,
    outgoing: std_mpsc::Sender<Vec<u8>>,
    configuration: Arc<Mutex<Value>>,
) {
    let mut reader = BufReader::new(stdout);
    while let Some(message) = read_message(&mut reader) {
        let Ok(value) = serde_json::from_slice::<Value>(&message) else {
            continue;
        };
        dispatch(value, &pending, &notifications, &outgoing, &configuration);
    }
    // The server is gone: fail everything still waiting.
    for (_, tx) in pending.lock().drain() {
        let _ = tx.send(Err(Error::Closed));
    }
}

/// One message of a server: an answer goes to whoever waits for it, a
/// request the app has to see and a notification go to the app, and the
/// rest get the answers every client gives.
fn dispatch(
    value: Value,
    pending: &Pending,
    notifications: &mpsc::UnboundedSender<Notification>,
    outgoing: &std_mpsc::Sender<Vec<u8>>,
    configuration: &Mutex<Value>,
) {
    let id = value.get("id").filter(|id| !id.is_null()).cloned();
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match (id, method) {
        // A response to one of our requests.
        (Some(id), None) => {
            let Some(tx) = id
                .as_i64()
                .and_then(|id| pending.lock().remove(&(id as i32)))
            else {
                return;
            };
            let result = match value.get("error").filter(|error| !error.is_null()) {
                Some(err) => Err(Error::Server {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("request failed")
                        .to_string(),
                }),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(result);
        }
        // A request from the server. Edits go to the app, which knows the
        // open documents; the rest get the answers every client gives.
        (Some(id), Some(method)) if method == "workspace/applyEdit" => {
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let _ = notifications.unbounded_send(Notification {
                method,
                params,
                id: Some(id),
            });
        }
        (Some(id), Some(method)) => {
            let result = match method.as_str() {
                "workspace/configuration" => {
                    let settings = configuration.lock();
                    Value::Array(
                        value
                            .pointer("/params/items")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .map(|item| section(&settings, item["section"].as_str()))
                            .collect(),
                    )
                }
                _ => Value::Null,
            };
            let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            if let Ok(body) = serde_json::to_vec(&reply) {
                let _ = outgoing.send(body);
            }
        }
        // Nobody listening any more is no reason to stop reading: the
        // server would block on a full pipe.
        (None, Some(method)) => {
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let _ = notifications.unbounded_send(Notification {
                method,
                params,
                id: None,
            });
        }
        (None, None) => {}
    }
}

/// Reads one `Content-Length`-framed message.
/// The part of `settings` a server asks for: all of it, the value under
/// the section's name, or the value its dotted path leads to.
fn section(settings: &Value, name: Option<&str>) -> Value {
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return settings.clone();
    };
    if let Some(found) = settings.get(name) {
        return found.clone();
    }
    name.split('.')
        .try_fold(settings, |value, key| value.get(key))
        .cloned()
        .unwrap_or(Value::Null)
}

fn read_message(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    Some(body)
}

/// The kinds of words the editor has a color for, as the protocol names
/// them.
pub const SEMANTIC_TOKEN_TYPES: &[&str] = &[
    "namespace",
    "type",
    "class",
    "enum",
    "interface",
    "struct",
    "typeParameter",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "event",
    "function",
    "method",
    "macro",
    "keyword",
    "modifier",
    "comment",
    "string",
    "number",
    "regexp",
    "operator",
    "decorator",
];

fn client_capabilities() -> ClientCapabilities {
    // Written as JSON: the typed structs are deeply nested and this reads
    // closer to the spec.
    let value = json!({
        "general": { "positionEncodings": ["utf-8", "utf-16"] },
        "textDocument": {
            "synchronization": { "didSave": true, "dynamicRegistration": false },
            "completion": {
                "completionItem": {
                    "snippetSupport": true,
                    "documentationFormat": ["markdown", "plaintext"],
                    "resolveSupport": { "properties": ["documentation", "detail"] },
                    "insertReplaceSupport": false,
                    "labelDetailsSupport": true
                },
                "contextSupport": true
            },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "definition": { "linkSupport": true },
            "implementation": { "linkSupport": true },
            "documentHighlight": {},
            "typeDefinition": { "linkSupport": true },
            "references": {},
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            "rename": { "prepareSupport": true },
            "formatting": {},
            "publishDiagnostics": { "relatedInformation": false, "versionSupport": true },
            "signatureHelp": {
                "signatureInformation": { "parameterInformation": { "labelOffsetSupport": true } }
            },
            "inlayHint": {},
            "codeLens": {},
            "semanticTokens": {
                "requests": { "full": true },
                "tokenTypes": SEMANTIC_TOKEN_TYPES,
                "tokenModifiers": [],
                "formats": ["relative"],
                "overlappingTokenSupport": false,
                "multilineTokenSupport": false
            },
            "codeAction": {
                "resolveSupport": { "properties": ["edit"] },
                "dataSupport": true,
                "codeActionLiteralSupport": {
                    "codeActionKind": { "valueSet": ["", "quickfix", "refactor", "source"] }
                }
            }
        },
        "workspace": {
            "workspaceFolders": true,
            "symbol": {},
            "configuration": true,
            "applyEdit": true,
            "workspaceEdit": { "documentChanges": true }
        },
        "window": { "workDoneProgress": true },
        // The commands of rust-analyzer's lenses that the editor does
        // itself: without them named here it offers no such lens.
        "experimental": { "commands": { "commands": [
            "rust-analyzer.runSingle", "rust-analyzer.showReferences"
        ] } }
    });
    serde_json::from_value(value).unwrap_or_default()
}

pub fn path_to_uri(path: &Path) -> Uri {
    let url = url::Url::from_file_path(path)
        .or_else(|_| url::Url::from_directory_path(path))
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", path.display()));
    Uri::from_str(&url).unwrap_or_else(|_| Uri::from_str("file:///").unwrap())
}

pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_section_of_the_settings() {
        let settings = json!({
            "css": { "lint": { "level": 2 } },
            "vue.inlayHints.missingProps": true
        });
        assert_eq!(section(&settings, None), settings);
        assert_eq!(section(&settings, Some("")), settings);
        assert_eq!(
            section(&settings, Some("css")),
            json!({ "lint": { "level": 2 } })
        );
        assert_eq!(section(&settings, Some("css.lint.level")), json!(2));
        // A key that itself has dots wins over a path.
        assert_eq!(
            section(&settings, Some("vue.inlayHints.missingProps")),
            json!(true)
        );
        assert_eq!(section(&settings, Some("less")), Value::Null);
        assert_eq!(section(&Value::Null, Some("css")), Value::Null);
    }

    #[test]
    fn a_server_that_is_not_a_process_speaks_through_its_link() {
        use futures::{StreamExt, executor::block_on};
        // What the app says is given to the other end, which answers
        // through the link: here it is this test.
        let (said, hears) = std_mpsc::channel::<Value>();
        let (server, link, mut notes) = LanguageServer::linked("demo", move |message| {
            let _ = said.send(message);
        })
        .unwrap();
        let next = || {
            hears
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap()
        };

        // The handshake sends nothing until it is waited for, so it is
        // waited for on a thread of its own while this one answers.
        std::thread::scope(|scope| {
            let starting =
                scope.spawn(|| block_on(server.initialize(Path::new("/tmp/project"), None)));
            let asked = next();
            assert_eq!(asked["method"], "initialize");
            assert_eq!(asked["params"]["rootUri"], "file:///tmp/project");
            link.receive(json!({ "jsonrpc": "2.0", "id": asked["id"], "result": {
                "capabilities": { "hoverProvider": true },
            } }));
            starting.join().unwrap().unwrap();
        });
        assert_eq!(next()["method"], "initialized");
        assert!(server.capabilities().hover_provider.is_some());
        assert_eq!(server.encoding(), Encoding::Utf16);

        // A request is answered, or refused in the server's words.
        let asking = server.request::<lsp_types::request::Shutdown>(());
        let asked = next();
        link.receive(json!({ "jsonrpc": "2.0", "id": asked["id"], "error": {
            "code": -32000, "message": "not now",
        } }));
        assert!(
            matches!(block_on(asking), Err(Error::Server { message, .. }) if message == "not now")
        );

        // What the server says of its own reaches the app, and what it
        // asks that every client answers is answered.
        link.receive(
            json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///a", "diagnostics": [] } }),
        );
        let note = block_on(notes.next()).unwrap();
        assert_eq!(note.method, "textDocument/publishDiagnostics");
        assert_eq!(note.params["uri"], "file:///a");
        server.set_configuration(json!({ "demo": { "level": 3 } }));
        assert_eq!(next()["method"], "workspace/didChangeConfiguration");
        link.receive(
            json!({ "jsonrpc": "2.0", "id": 7, "method": "workspace/configuration",
            "params": { "items": [{ "section": "demo.level" }] } }),
        );
        assert_eq!(next(), json!({ "jsonrpc": "2.0", "id": 7, "result": [3] }));

        // The link gone, whoever waits is told there is no server.
        let asking = server.request::<lsp_types::request::Shutdown>(());
        next();
        drop(link);
        assert!(matches!(block_on(asking), Err(Error::Closed)));
    }

    #[test]
    fn frames_round_trip() {
        let body = br#"{"jsonrpc":"2.0","method":"x"}"#;
        let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        framed.extend_from_slice(body);
        framed.extend_from_slice(b"Content-Length: 2\r\nContent-Type: x\r\n\r\n{}");
        let mut reader = Cursor::new(framed);
        assert_eq!(read_message(&mut reader).unwrap(), body);
        assert_eq!(read_message(&mut reader).unwrap(), b"{}");
        assert!(read_message(&mut reader).is_none());
    }

    #[test]
    fn uris_round_trip_with_spaces() {
        let path = Path::new("/tmp/my project/src/main.rs");
        let uri = path_to_uri(path);
        assert_eq!(uri.as_str(), "file:///tmp/my%20project/src/main.rs");
        assert_eq!(uri_to_path(&uri).unwrap(), path);
    }
}
