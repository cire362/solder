//! A client for Model Context Protocol servers: programs that give a model
//! tools of their own (a database to query, an issue tracker to read).
//!
//! A server is a process the editor starts and talks to on its input and
//! output, one JSON message a line, or one that is somewhere else and is
//! talked to over HTTP: each message is a request to one address, answered
//! at once or as a stream of events (the protocol's "streamable HTTP").
//!
//! A server has tools, which are listed once it is ready and called by
//! name, and may have prompts, which are texts it writes for the user to
//! send, and resources, which are things of its own to read. What the
//! server asks of the client is answered as little as the protocol allows:
//! a ping is answered, anything else is refused.
//!
//! Everything here blocks, for as long as the server takes to answer. Call
//! it from a background executor.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use serde_json::{Value, json};

/// The version of the protocol this client speaks. A server answers with
/// the one it will use, which may be older; tools are the same in all.
const PROTOCOL: &str = "2025-06-18";
/// What a tool's answer is cut to: it goes to a model, whole.
const ANSWER_LIMIT: usize = 60_000;
/// What is kept of what a server writes to its error output.
const STDERR_KEPT: usize = 2_000;

/// How to start a server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    /// The whole environment: the user's own, with the server's variables.
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

/// Where a server that is not started here is: the address every message
/// goes to, and what to send with each (a key, mostly).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remote {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// A text a server writes for the user to send: its name there, what it
/// is for, and what it has to be told first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub name: String,
    pub description: String,
    pub arguments: Vec<Argument>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argument {
    pub name: String,
    pub description: String,
    pub required: bool,
}

/// Something of a server's own to read: a file, a table, a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    pub description: String,
}

/// A tool a server offers: its name there, what it is for, and the JSON
/// Schema of its arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

type Answer = Result<Value, String>;
type Waiting = Arc<Mutex<HashMap<u64, mpsc::Sender<Answer>>>>;

pub struct Server {
    link: Link,
    next: AtomicU64,
    /// What the server calls itself, and what it says about using it.
    pub name: String,
    pub instructions: Option<String>,
    /// Whether it says it has prompts, and resources.
    pub has_prompts: bool,
    pub has_resources: bool,
}

/// How a server is talked to.
enum Link {
    Process(Process),
    Http(Http),
}

/// A server started here.
struct Process {
    child: Mutex<Child>,
    /// Shared with the thread that answers the server's own questions,
    /// and closed for both at once when the server is stopped.
    input: Arc<Mutex<Option<ChildStdin>>>,
    waiting: Waiting,
    stderr: Arc<Mutex<String>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The last line of what a server wrote to its error output: usually the
/// reason it would not start.
fn last_line(stderr: &Mutex<String>) -> String {
    lock(stderr)
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string()
}

impl Process {
    fn spawn(launch: &Launch) -> Result<Self, String> {
        let mut command = Command::new(&launch.program);
        command
            .args(&launch.args)
            .env_clear()
            .envs(launch.env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &launch.cwd {
            command.current_dir(cwd);
        }
        // A group of its own, so that what it starts stops with it.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command
            .spawn()
            .map_err(|e| format!("Could not start {}: {e}", launch.program))?;
        let input = Arc::new(Mutex::new(child.stdin.take()));
        let output = child.stdout.take().ok_or("The server has no output")?;
        let errors = child.stderr.take();
        let waiting: Waiting = Arc::default();
        let stderr = Arc::new(Mutex::new(String::new()));
        let (answers, asked) = mpsc::channel::<Value>();
        {
            let waiting = waiting.clone();
            std::thread::Builder::new()
                .name("mcp-reader".into())
                .spawn(move || read(output, waiting, answers))
                .map_err(|e| e.to_string())?;
        }
        if let Some(errors) = errors {
            let kept = stderr.clone();
            std::thread::Builder::new()
                .name("mcp-stderr".into())
                .spawn(move || keep(errors, kept))
                .map_err(|e| e.to_string())?;
        }
        // What the server asks of the client goes back from a thread of its
        // own: the reader must not wait on the writer.
        {
            let input = input.clone();
            std::thread::Builder::new()
                .name("mcp-answers".into())
                .spawn(move || {
                    for message in asked {
                        let _ = write(&input, &message);
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(Self {
            child: Mutex::new(child),
            input,
            waiting,
            stderr,
        })
    }

    fn send(&self, message: &Value) -> Result<(), String> {
        write(&self.input, message).map_err(|_| self.gone())
    }

    /// Why the server is not there to answer, in its own words if it left
    /// any.
    fn gone(&self) -> String {
        // Its last words may still be on their way.
        if lock(&self.stderr).is_empty() {
            std::thread::sleep(Duration::from_millis(100));
        }
        match last_line(&self.stderr) {
            line if line.is_empty() => "The server stopped".into(),
            line => format!("The server stopped: {line}"),
        }
    }

    fn request(&self, id: u64, method: &str, params: Value, patience: Duration) -> Answer {
        let (tx, rx) = mpsc::channel();
        // Registered before it is sent, so a fast answer is not missed.
        lock(&self.waiting).insert(id, tx);
        let sent = self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        }));
        if let Err(error) = sent {
            lock(&self.waiting).remove(&id);
            return Err(error);
        }
        match rx.recv_timeout(patience) {
            Ok(answer) => answer,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lock(&self.waiting).remove(&id);
                Err(late(method, patience))
            }
            // The reader ended: the server is gone.
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.gone()),
        }
    }

    /// Its input is closed, which is how a server is asked to leave, and
    /// what is left of its group a moment later is ended.
    fn stop(&self) {
        lock(&self.input).take();
        let mut child = lock(&self.child);
        if cfg!(unix) {
            let script =
                r#"( sleep 1; kill -TERM "-$1"; sleep 2; kill -KILL "-$1" ) >/dev/null 2>&1 &"#;
            let _ = Command::new("sh")
                .args(["-c", script, "sh", &child.id().to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        } else {
            let _ = child.kill();
        }
        let _ = child.try_wait();
    }
}

fn late(method: &str, patience: Duration) -> String {
    format!(
        "The server did not answer {method} in {} s",
        patience.as_secs().max(1)
    )
}

/// What a message that answers a request says: its result, or the
/// server's words for why not.
fn answer_of(message: &Value) -> Answer {
    match message.get("error").filter(|e| !e.is_null()) {
        Some(error) => Err(error["message"]
            .as_str()
            .unwrap_or("the server refused")
            .to_string()),
        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
    }
}

/// What the client says to something a server asks of it.
fn answer_to(id: &Value, method: &str) -> Value {
    if method == "ping" {
        json!({ "jsonrpc": "2.0", "id": id, "result": {} })
    } else {
        json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32601, "message": "Solder does not do this" },
        })
    }
}

/// A server reached over HTTP. Each message is a request of its own to
/// the same address; the server names the conversation in its first
/// answer, and that name goes with every message after.
struct Http {
    remote: Remote,
    session: Mutex<Option<String>>,
    /// The version of the protocol the two agreed on, sent with every
    /// message once the greeting is over.
    version: Mutex<Option<String>>,
    open: AtomicBool,
}

/// One event of a stream of them, taken off the front of `buffer` if a
/// whole one is there: the lines of its `data`, joined.
fn take_event(buffer: &mut String) -> Option<String> {
    let normal = buffer.replace("\r\n", "\n");
    let end = normal.find("\n\n")?;
    let event = normal[..end].to_string();
    *buffer = normal[end + 2..].to_string();
    let data: Vec<&str> = event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect();
    Some(data.join("\n"))
}

impl Http {
    /// No timeout of its own for an answer: a tool may think for longer
    /// than any other request, and whoever asks says how long to wait.
    fn client() -> &'static reqwest::Client {
        static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        CLIENT.get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .user_agent(concat!("Solder/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("HTTP client")
        })
    }

    fn post(&self, message: &Value) -> reqwest::RequestBuilder {
        let mut request = Self::client()
            .post(&self.remote.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json");
        for (name, value) in &self.remote.headers {
            request = request.header(name, value);
        }
        if let Some(session) = lock(&self.session).as_deref() {
            request = request.header("Mcp-Session-Id", session);
        }
        if let Some(version) = lock(&self.version).as_deref() {
            request = request.header("MCP-Protocol-Version", version);
        }
        request.body(message.to_string())
    }

    /// Why a request did not go through, from the status of its answer.
    fn refused(&self, status: u16) -> String {
        match status {
            401 | 403 => format!(
                "The server asks who is calling ({status}). Give it a key under headers in settings.json"
            ),
            404 if lock(&self.session).is_some() => {
                "The server no longer knows this conversation".into()
            }
            404 | 405 => "No context server answers at this address".into(),
            status => format!("The server answered {status}"),
        }
    }

    /// Sends a message nothing is expected back for.
    async fn tell(&self, message: Value) -> Result<(), String> {
        let response = self
            .post(&message)
            .send()
            .await
            .map_err(|e| format!("Could not reach the server: {e}"))?;
        match response.status() {
            status if status.is_success() => Ok(()),
            status => Err(self.refused(status.as_u16())),
        }
    }

    /// Sends a request and reads until its answer is there: the body of
    /// the response, or one event of a stream. What the server asks in
    /// that stream is answered on the way.
    async fn ask(&self, id: u64, message: Value) -> Answer {
        let mut response = self
            .post(&message)
            .send()
            .await
            .map_err(|e| format!("Could not reach the server: {e}"))?;
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
        {
            *lock(&self.session) = Some(session.to_string());
        }
        let status = response.status();
        if !status.is_success() {
            return Err(self.refused(status.as_u16()));
        }
        let is_ours =
            |message: &Value| message["id"].as_u64() == Some(id) && message["method"].is_null();
        let stream = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|kind| kind.starts_with("text/event-stream"));
        if !stream {
            let unreadable = || "The server's answer is not one this protocol has".to_string();
            let bytes = response.bytes().await.map_err(|_| unreadable())?;
            let body: Value = serde_json::from_slice(&bytes).map_err(|_| unreadable())?;
            // One message, or several sent together.
            let messages = match &body {
                Value::Array(messages) => messages.as_slice(),
                one => std::slice::from_ref(one),
            };
            return messages
                .iter()
                .find(|message| is_ours(message))
                .map(answer_of)
                .unwrap_or_else(|| Err("The server answered something else".into()));
        }
        let mut buffer = String::new();
        loop {
            let chunk = response
                .chunk()
                .await
                .map_err(|e| format!("The server's answer broke off: {e}"))?;
            let Some(chunk) = chunk else {
                return Err("The server ended its answer before giving it".into());
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(data) = take_event(&mut buffer) {
                let Ok(message) = serde_json::from_str::<Value>(&data) else {
                    continue;
                };
                if is_ours(&message) {
                    return answer_of(&message);
                }
                let asked = message.get("id").filter(|id| !id.is_null());
                if let (Some(asked), Some(method)) = (asked, message["method"].as_str()) {
                    let _ = self.tell(answer_to(asked, method)).await;
                }
            }
        }
    }

    fn request(&self, id: u64, method: &str, params: Value, patience: Duration) -> Answer {
        if !self.open.load(Ordering::Relaxed) {
            return Err("The server was stopped".into());
        }
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        crate::runtime().block_on(async {
            match tokio::time::timeout(patience, self.ask(id, message)).await {
                Ok(answer) => answer,
                Err(_) => Err(late(method, patience)),
            }
        })
    }

    fn send(&self, message: &Value) -> Result<(), String> {
        let message = message.clone();
        crate::runtime().block_on(async {
            match tokio::time::timeout(Duration::from_secs(20), self.tell(message)).await {
                Ok(told) => told,
                Err(_) => Err("The server did not take the message".into()),
            }
        })
    }

    /// Tells the server the conversation is over, if it named one.
    fn stop(&self) {
        if !self.open.swap(false, Ordering::Relaxed) {
            return;
        }
        let Some(session) = lock(&self.session).take() else {
            return;
        };
        let mut request = Self::client()
            .delete(&self.remote.url)
            .header("Mcp-Session-Id", session);
        for (name, value) in &self.remote.headers {
            request = request.header(name, value);
        }
        crate::runtime().block_on(async {
            let _ = tokio::time::timeout(Duration::from_secs(3), request.send()).await;
        });
    }
}

impl Server {
    /// Starts the server and greets it. `patience` is how long it may take
    /// to answer the greeting: a server started with `npx` downloads itself
    /// first.
    pub fn start(launch: &Launch, patience: Duration) -> Result<Self, String> {
        Self::greet(Link::Process(Process::spawn(launch)?), patience)
    }

    /// Greets a server that is somewhere else.
    pub fn connect(remote: &Remote, patience: Duration) -> Result<Self, String> {
        if !(remote.url.starts_with("http://") || remote.url.starts_with("https://")) {
            return Err(format!("{} is not an http address", remote.url));
        }
        let http = Http {
            remote: remote.clone(),
            session: Mutex::new(None),
            version: Mutex::new(None),
            open: AtomicBool::new(true),
        };
        Self::greet(Link::Http(http), patience)
    }

    fn greet(link: Link, patience: Duration) -> Result<Self, String> {
        let mut server = Self {
            link,
            next: AtomicU64::new(1),
            name: String::new(),
            instructions: None,
            has_prompts: false,
            has_resources: false,
        };
        let greeting = server.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "Solder", "version": env!("CARGO_PKG_VERSION") },
            }),
            patience,
        )?;
        server.name = greeting["serverInfo"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        server.instructions = greeting["instructions"]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        server.has_prompts = greeting["capabilities"]["prompts"].is_object();
        server.has_resources = greeting["capabilities"]["resources"].is_object();
        if let Link::Http(http) = &server.link {
            *lock(&http.version) = greeting["protocolVersion"].as_str().map(str::to_string);
        }
        server.notify("notifications/initialized")?;
        Ok(server)
    }

    fn notify(&self, method: &str) -> Result<(), String> {
        let message = json!({ "jsonrpc": "2.0", "method": method });
        match &self.link {
            Link::Process(process) => process.send(&message),
            Link::Http(http) => http.send(&message),
        }
    }

    fn request(&self, method: &str, params: Value, patience: Duration) -> Answer {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        match &self.link {
            Link::Process(process) => process.request(id, method, params, patience),
            Link::Http(http) => http.request(id, method, params, patience),
        }
    }

    /// Everything a list of the server's holds under `key`. A long list
    /// comes in pages; a server that keeps giving a next page is cut off.
    fn listed(&self, method: &str, key: &str, patience: Duration) -> Result<Vec<Value>, String> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..50 {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self.request(method, params, patience)?;
            all.extend(page[key].as_array().into_iter().flatten().cloned());
            cursor = page["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(all)
    }

    /// Every tool the server offers.
    pub fn tools(&self, patience: Duration) -> Result<Vec<Tool>, String> {
        let tools = self.listed("tools/list", "tools", patience)?;
        Ok(tools
            .iter()
            .filter_map(|tool| {
                Some(Tool {
                    name: tool["name"].as_str()?.to_string(),
                    description: text_of(&tool["description"]),
                    schema: match &tool["inputSchema"] {
                        schema @ Value::Object(_) => schema.clone(),
                        _ => json!({ "type": "object", "properties": {} }),
                    },
                })
            })
            .collect())
    }

    /// The prompts the server has; none for one that says it has none.
    pub fn prompts(&self, patience: Duration) -> Result<Vec<Prompt>, String> {
        if !self.has_prompts {
            return Ok(Vec::new());
        }
        let prompts = self.listed("prompts/list", "prompts", patience)?;
        Ok(prompts
            .iter()
            .filter_map(|prompt| {
                Some(Prompt {
                    name: prompt["name"].as_str()?.to_string(),
                    description: text_of(&prompt["description"]),
                    arguments: prompt["arguments"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|argument| {
                            Some(Argument {
                                name: argument["name"].as_str()?.to_string(),
                                description: text_of(&argument["description"]),
                                required: argument["required"].as_bool().unwrap_or(false),
                            })
                        })
                        .collect(),
                })
            })
            .collect())
    }

    /// The text of a prompt, written by the server from what it is told:
    /// what it would have the user say, each message after the other.
    pub fn prompt(
        &self,
        name: &str,
        arguments: &[(String, String)],
        patience: Duration,
    ) -> Result<String, String> {
        let arguments: serde_json::Map<String, Value> = arguments
            .iter()
            .map(|(name, value)| (name.clone(), Value::String(value.clone())))
            .collect();
        let result = self.request(
            "prompts/get",
            json!({ "name": name, "arguments": arguments }),
            patience,
        )?;
        let mut text = String::new();
        for message in result["messages"].as_array().into_iter().flatten() {
            // One part, or several.
            let parts = match &message["content"] {
                Value::Array(parts) => parts.as_slice(),
                one => std::slice::from_ref(one),
            };
            for part in parts {
                join(&mut text, &part_text(part), "\n\n");
            }
        }
        Ok(cut(text))
    }

    /// What the server has to read; none for one that says it has none.
    pub fn resources(&self, patience: Duration) -> Result<Vec<Resource>, String> {
        if !self.has_resources {
            return Ok(Vec::new());
        }
        let resources = self.listed("resources/list", "resources", patience)?;
        Ok(resources
            .iter()
            .filter_map(|resource| {
                let uri = resource["uri"].as_str()?.to_string();
                Some(Resource {
                    name: match text_of(&resource["name"]) {
                        name if name.is_empty() => uri.clone(),
                        name => name,
                    },
                    description: text_of(&resource["description"]),
                    uri,
                })
            })
            .collect())
    }

    /// Reads one of them, as text. What is not text is said to be there.
    pub fn read(&self, uri: &str, patience: Duration) -> Result<String, String> {
        let result = self.request("resources/read", json!({ "uri": uri }), patience)?;
        let mut text = String::new();
        for content in result["contents"].as_array().into_iter().flatten() {
            let piece = match content["text"].as_str() {
                Some(piece) => piece.to_string(),
                None => format!(
                    "[{}: not text]",
                    content["mimeType"].as_str().unwrap_or("data")
                ),
            };
            join(&mut text, &piece, "\n");
        }
        Ok(cut(text))
    }

    /// Calls a tool. The answer is its text, and whether the tool says it
    /// failed; `Err` is for a call that did not get through at all.
    pub fn call(
        &self,
        tool: &str,
        arguments: Value,
        patience: Duration,
    ) -> Result<(String, bool), String> {
        let arguments = match arguments {
            Value::Object(_) => arguments,
            _ => json!({}),
        };
        let result = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
            patience,
        )?;
        let mut text = String::new();
        for part in result["content"].as_array().into_iter().flatten() {
            join(&mut text, &part_text(part), "\n");
        }
        // A tool that answers with data only.
        if text.is_empty() && !result["structuredContent"].is_null() {
            text = result["structuredContent"].to_string();
        }
        let text = cut(text);
        Ok((text, result["isError"].as_bool().unwrap_or(false)))
    }

    /// Whether the server is still there to ask: its process, or for one
    /// that is somewhere else, the conversation with it.
    pub fn is_running(&self) -> bool {
        match &self.link {
            Link::Process(process) => matches!(lock(&process.child).try_wait(), Ok(None)),
            Link::Http(http) => http.open.load(Ordering::Relaxed),
        }
    }

    /// Stops the server, or ends the conversation with one that is
    /// somewhere else.
    pub fn stop(&self) {
        match &self.link {
            Link::Process(process) => process.stop(),
            Link::Http(http) => http.stop(),
        }
    }
}

/// A text a server gives, without the space around it.
fn text_of(value: &Value) -> String {
    value.as_str().unwrap_or_default().trim().to_string()
}

/// What one part of an answer says, as text.
fn part_text(part: &Value) -> String {
    match part["type"].as_str() {
        Some("text") => part["text"].as_str().unwrap_or_default().to_string(),
        Some("resource") => part["resource"]["text"]
            .as_str()
            .or(part["resource"]["uri"].as_str())
            .unwrap_or_default()
            .to_string(),
        Some("resource_link") => part["uri"].as_str().unwrap_or_default().to_string(),
        // Pictures and sound are not something to hand a model as text;
        // it is told they were there.
        Some(other) => format!("[{other}]"),
        None => String::new(),
    }
}

fn join(text: &mut String, piece: &str, between: &str) {
    if !text.is_empty() && !piece.is_empty() {
        text.push_str(between);
    }
    text.push_str(piece);
}

/// A text no longer than a model should be handed whole.
fn cut(mut text: String) -> String {
    if text.len() > ANSWER_LIMIT {
        let mut cut = ANSWER_LIMIT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[cut here: the answer was longer]");
    }
    text
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn write(input: &Mutex<Option<ChildStdin>>, message: &Value) -> Result<(), String> {
    let guard = lock(input);
    let Some(mut input) = guard.as_ref() else {
        return Err("stopped".into());
    };
    let mut line = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    line.push(b'\n');
    input
        .write_all(&line)
        .and_then(|()| input.flush())
        .map_err(|e| e.to_string())
}

/// Reads the server's messages: answers go to whoever waits for them, and
/// what the server asks is answered through `answers`.
fn read(output: impl Read, waiting: Waiting, answers: mpsc::Sender<Value>) {
    let reader = BufReader::new(output);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            // Something a server printed that is not the protocol.
            continue;
        };
        let id = message.get("id").filter(|id| !id.is_null());
        match (id, message["method"].as_str()) {
            (Some(id), None) => {
                let Some(tx) = id.as_u64().and_then(|id| lock(&waiting).remove(&id)) else {
                    continue;
                };
                let _ = tx.send(answer_of(&message));
            }
            (Some(id), Some(method)) => {
                let _ = answers.send(answer_to(id, method));
            }
            // A notification: nothing to do with any of them yet.
            _ => {}
        }
    }
    // Whoever still waits learns that no answer will come.
    lock(&waiting).clear();
}

fn keep(errors: impl Read, kept: Arc<Mutex<String>>) {
    let reader = BufReader::new(errors);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let mut kept = lock(&kept);
        kept.push_str(&line);
        kept.push('\n');
        if kept.len() > STDERR_KEPT {
            let mut cut = kept.len() - STDERR_KEPT;
            while !kept.is_char_boundary(cut) {
                cut += 1;
            }
            kept.drain(..cut);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_process(mode: &str) -> Launch {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mock_mcp.py");
        Launch {
            program: "python3".into(),
            args: vec![script.into()],
            env: vec![
                ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
                ("MOCK_MCP".into(), mode.into()),
            ],
            cwd: None,
        }
    }

    const SOON: Duration = Duration::from_secs(10);

    #[test]
    fn a_server_is_greeted_lists_its_tools_and_runs_them() {
        let server = Server::start(&mock_process(""), SOON).unwrap();
        assert_eq!(server.name, "mock");
        assert_eq!(server.instructions.as_deref(), Some("Ask before you look."));
        // Two pages of tools, with the schema of each.
        let tools = server.tools(SOON).unwrap();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["echo", "fail", "slow", "data"]);
        assert_eq!(tools[0].description, "Says it back.");
        assert_eq!(tools[0].schema["required"][0], "text");
        // One with no schema still takes an object.
        assert_eq!(tools[3].schema["type"], "object");

        // Text comes back joined; a tool's own failure is an answer, not
        // an error of the call.
        let said = server.call("echo", json!({ "text": "hello" }), SOON);
        assert_eq!(said, Ok(("hello\n[image]".into(), false)));
        let failed = server.call("fail", json!({}), SOON);
        assert_eq!(failed, Ok(("no such row".into(), true)));
        assert_eq!(
            server.call("data", Value::Null, SOON),
            Ok((r#"{"rows":2}"#.into(), false))
        );
        // A tool the server does not have is refused in its words, and one
        // that takes too long is not waited for. The server answers the
        // next call all the same.
        assert_eq!(
            server.call("absent", json!({}), SOON),
            Err("Unknown tool: absent".into())
        );
        let late = server.call("slow", json!({}), Duration::from_millis(200));
        assert!(
            late.unwrap_err()
                .starts_with("The server did not answer tools/call")
        );
        assert_eq!(
            server
                .call("echo", json!({ "text": "again" }), SOON)
                .unwrap()
                .0,
            "again\n[image]"
        );
        // It pinged the client while it was greeted and got its answer.
        assert_eq!(
            server
                .call("echo", json!({ "text": "pinged?" }), SOON)
                .unwrap()
                .0,
            "pinged? yes\n[image]"
        );
        assert!(server.is_running());
        server.stop();
        assert!(server.call("echo", json!({}), SOON).is_err());
    }

    /// The stand-in server reached over HTTP, on a port of its own, and
    /// the file it writes down what it is told in. Ended when dropped.
    struct Remotely(std::process::Child, String, std::path::PathBuf);

    impl Drop for Remotely {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn remotely(name: &str) -> Remotely {
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/mock_mcp_http.py"
        );
        let log =
            std::env::temp_dir().join(format!("solder-mcp-http-{name}-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let mut child = Command::new("python3")
            .arg(script)
            .env("MOCK_MCP_LOG", &log)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut port = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        Remotely(child, format!("http://127.0.0.1:{}/mcp", port.trim()), log)
    }

    #[test]
    fn a_server_somewhere_else_is_reached_over_http() {
        let mock = remotely("talk");
        let key = vec![("Authorization".to_string(), "Bearer t".to_string())];
        let remote = Remote {
            url: mock.1.clone(),
            headers: key.clone(),
        };
        // Greeted with the key the settings give; from then on every
        // message carries the name the server gave the conversation.
        let server = Server::connect(&remote, SOON).unwrap();
        assert_eq!(server.name, "mock-http");
        assert!(server.has_prompts && server.has_resources);
        // The list comes as a stream of events, sent in pieces, with a
        // question of the server's own before the answer.
        let tools = server.tools(SOON).unwrap();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["echo", "slow"]);
        // A plain answer; and the server's ping was answered on the way.
        let said = server.call("echo", json!({ "text": "pinged?" }), SOON);
        assert_eq!(said, Ok(("pinged? yes".into(), false)));
        assert_eq!(
            server.call("absent", json!({}), SOON),
            Err("Unknown: tools/call".into())
        );
        let late = server.call("slow", json!({}), Duration::from_millis(200));
        assert!(
            late.unwrap_err()
                .starts_with("The server did not answer tools/call")
        );
        assert_eq!(
            server
                .call("echo", json!({ "text": "again" }), SOON)
                .unwrap()
                .0,
            "again"
        );
        // Stopped, the server is told the conversation is over, and
        // nothing more is asked of it.
        assert!(server.is_running());
        server.stop();
        assert!(!server.is_running());
        assert_eq!(
            server.call("echo", json!({}), SOON),
            Err("The server was stopped".into())
        );
        let told = std::fs::read_to_string(&mock.2).unwrap();
        assert!(
            told.contains(r#"["told", "notifications/initialized"]"#),
            "{told}"
        );
        assert!(told.contains(r#"["ended", "talk-1"]"#), "{told}");

        // Without the key, and at an address nothing answers at, it says
        // what to do about it.
        let nameless = Remote {
            url: mock.1.clone(),
            headers: Vec::new(),
        };
        let error = Server::connect(&nameless, SOON).err().unwrap();
        assert!(
            error.contains("401") && error.contains("headers"),
            "{error}"
        );
        let elsewhere = Remote {
            url: mock.1.replace("/mcp", "/other"),
            headers: key,
        };
        assert_eq!(
            Server::connect(&elsewhere, SOON).err().unwrap(),
            "No context server answers at this address"
        );
        let nowhere = Remote {
            url: "http://127.0.0.1:1/mcp".into(),
            ..Default::default()
        };
        let error = Server::connect(&nowhere, SOON).err().unwrap();
        assert!(error.starts_with("Could not reach the server"), "{error}");
        let odd = Remote {
            url: "ftp://example.com".into(),
            ..Default::default()
        };
        assert!(Server::connect(&odd, SOON).is_err());
    }

    #[test]
    fn a_server_has_prompts_to_send_and_resources_to_read() {
        let mock = remotely("more");
        let remote = Remote {
            url: mock.1.clone(),
            headers: vec![("Authorization".into(), "Bearer t".into())],
        };
        let server = Server::connect(&remote, SOON).unwrap();
        // Prompts, with what each has to be told.
        let prompts = server.prompts(SOON).unwrap();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].name, "review");
        assert_eq!(prompts[0].description, "Reviews a change.");
        let arguments: Vec<_> = prompts[0]
            .arguments
            .iter()
            .map(|a| (a.name.as_str(), a.required))
            .collect();
        assert_eq!(arguments, [("branch", true), ("tone", false)]);
        assert!(prompts[1].arguments.is_empty());
        // The text of one, written by the server from what it is told.
        let told = [("branch".to_string(), "main".to_string())];
        assert_eq!(
            server.prompt("review", &told, SOON),
            Ok("Review main.\n\nBe kind.".into())
        );
        assert!(server.prompt("absent", &[], SOON).is_err());

        // Resources: listed, read as text, and said to be there when
        // they are not text.
        let resources = server.resources(SOON).unwrap();
        let listed: Vec<_> = resources
            .iter()
            .map(|r| (r.uri.as_str(), r.name.as_str()))
            .collect();
        assert_eq!(
            listed,
            [("notes://today", "Today"), ("notes://logo", "notes://logo")]
        );
        assert_eq!(
            server.read("notes://today", SOON),
            Ok("Ship the layout.".into())
        );
        assert_eq!(
            server.read("notes://logo", SOON),
            Ok("[image/png: not text]".into())
        );
        assert!(server.read("notes://absent", SOON).is_err());

        // A server that does not say it has them is not asked for them.
        let plain = Server::start(&mock_process(""), SOON).unwrap();
        assert!(!plain.has_prompts && !plain.has_resources);
        assert_eq!(plain.prompts(SOON), Ok(Vec::new()));
        assert_eq!(plain.resources(SOON), Ok(Vec::new()));
    }

    #[test]
    fn events_are_taken_off_a_stream_as_they_are_complete() {
        let mut buffer =
            String::from(": note\n\nevent: message\ndata: {\"a\":\ndata: 1}\r\n\r\ndata: half");
        assert_eq!(take_event(&mut buffer).as_deref(), Some(""));
        assert_eq!(take_event(&mut buffer).as_deref(), Some("{\"a\":\n1}"));
        assert_eq!(take_event(&mut buffer), None);
        buffer.push_str("\n\n");
        assert_eq!(take_event(&mut buffer).as_deref(), Some("half"));
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_server_that_does_not_start_says_why() {
        let missing = Launch {
            program: "/no/such/mcp-server".into(),
            ..Default::default()
        };
        let error = Server::start(&missing, SOON).err().unwrap();
        assert!(
            error.starts_with("Could not start /no/such/mcp-server"),
            "{error}"
        );
        // One that dies before it answers: its own last words.
        let error = Server::start(&mock_process("crash"), SOON).err().unwrap();
        assert_eq!(error, "The server stopped: DATABASE_URL is not set");
        // One that never answers.
        let error = Server::start(&mock_process("silent"), Duration::from_millis(300))
            .err()
            .unwrap();
        assert!(
            error.starts_with("The server did not answer initialize"),
            "{error}"
        );
    }
}
