//! A client for Model Context Protocol servers: programs that give a model
//! tools of their own (a database to query, an issue tracker to read).
//!
//! A server is a process the editor starts and talks to on its input and
//! output, one JSON message a line. Only tools are used: they are listed
//! once the server is ready and called by name. What the server asks of the
//! client is answered as little as the protocol allows: a ping is answered,
//! anything else is refused.
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
        atomic::{AtomicU64, Ordering},
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
    child: Mutex<Child>,
    /// Shared with the thread that answers the server's own questions,
    /// and closed for both at once when the server is stopped.
    input: Arc<Mutex<Option<ChildStdin>>>,
    waiting: Waiting,
    next: AtomicU64,
    stderr: Arc<Mutex<String>>,
    /// What the server calls itself, and what it says about using it.
    pub name: String,
    pub instructions: Option<String>,
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

impl Server {
    /// Starts the server and greets it. `patience` is how long it may take
    /// to answer the greeting: a server started with `npx` downloads itself
    /// first.
    pub fn start(launch: &Launch, patience: Duration) -> Result<Self, String> {
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
        let input = child.stdin.take();
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
        let mut server = Self {
            child: Mutex::new(child),
            input: Arc::new(Mutex::new(input)),
            waiting,
            next: AtomicU64::new(1),
            stderr,
            name: String::new(),
            instructions: None,
        };
        // What the server asks of the client goes back from a thread of its
        // own: the reader must not wait on the writer.
        {
            let input = server.input.clone();
            std::thread::Builder::new()
                .name("mcp-answers".into())
                .spawn(move || {
                    for message in asked {
                        let _ = write(&input, &message);
                    }
                })
                .map_err(|e| e.to_string())?;
        }
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
        server.notify("notifications/initialized")?;
        Ok(server)
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

    fn notify(&self, method: &str) -> Result<(), String> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }))
    }

    fn request(&self, method: &str, params: Value, patience: Duration) -> Answer {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
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
                Err(format!(
                    "The server did not answer {method} in {} s",
                    patience.as_secs().max(1)
                ))
            }
            // The reader ended: the server is gone.
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.gone()),
        }
    }

    /// Every tool the server offers.
    pub fn tools(&self, patience: Duration) -> Result<Vec<Tool>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        // A long list comes in pages; a server that keeps giving a next
        // page is cut off.
        for _ in 0..50 {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self.request("tools/list", params, patience)?;
            for tool in page["tools"].as_array().into_iter().flatten() {
                let Some(name) = tool["name"].as_str() else {
                    continue;
                };
                tools.push(Tool {
                    name: name.to_string(),
                    description: tool["description"]
                        .as_str()
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    schema: match &tool["inputSchema"] {
                        schema @ Value::Object(_) => schema.clone(),
                        _ => json!({ "type": "object", "properties": {} }),
                    },
                });
            }
            cursor = page["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
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
            let piece = match part["type"].as_str() {
                Some("text") => part["text"].as_str().unwrap_or_default().to_string(),
                Some("resource") => part["resource"]["text"]
                    .as_str()
                    .or(part["resource"]["uri"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                Some("resource_link") => part["uri"].as_str().unwrap_or_default().to_string(),
                // Pictures and sound are not something to hand a model as
                // text; it is told they were there.
                Some(other) => format!("[{other}]"),
                None => continue,
            };
            if !text.is_empty() && !piece.is_empty() {
                text.push('\n');
            }
            text.push_str(&piece);
        }
        // A tool that answers with data only.
        if text.is_empty() && !result["structuredContent"].is_null() {
            text = result["structuredContent"].to_string();
        }
        if text.len() > ANSWER_LIMIT {
            let mut cut = ANSWER_LIMIT;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("\n[cut here: the answer was longer]");
        }
        Ok((text, result["isError"].as_bool().unwrap_or(false)))
    }

    /// Whether the process is still there.
    pub fn is_running(&self) -> bool {
        matches!(lock(&self.child).try_wait(), Ok(None))
    }

    /// Stops the server: its input is closed, which is how one is asked to
    /// leave, and what is left of its group a moment later is ended.
    pub fn stop(&self) {
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
                let answer = match message.get("error").filter(|e| !e.is_null()) {
                    Some(error) => Err(error["message"]
                        .as_str()
                        .unwrap_or("the server refused")
                        .to_string()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(answer);
            }
            (Some(id), Some(method)) => {
                let answer = if method == "ping" {
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} })
                } else {
                    json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32601, "message": "Solder does not do this" },
                    })
                };
                let _ = answers.send(answer);
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

    fn mock(mode: &str) -> Launch {
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
        let server = Server::start(&mock(""), SOON).unwrap();
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
        let error = Server::start(&mock("crash"), SOON).err().unwrap();
        assert_eq!(error, "The server stopped: DATABASE_URL is not set");
        // One that never answers.
        let error = Server::start(&mock("silent"), Duration::from_millis(300))
            .err()
            .unwrap();
        assert!(
            error.starts_with("The server did not answer initialize"),
            "{error}"
        );
    }
}
