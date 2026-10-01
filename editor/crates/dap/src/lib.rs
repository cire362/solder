//! A Debug Adapter Protocol client.
//!
//! One connection per debug session, over TCP: js-debug serves every session
//! (the program, its child processes, a browser's pages) on one port, and
//! asks for a new connection with `startDebugging` for each child. One
//! reader and one writer thread per connection; requests return futures that
//! resolve with the response, and events and the adapter's own requests come
//! out of a channel the app polls.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
        mpsc as std_mpsc,
    },
    time::{Duration, Instant},
};

use futures::channel::{mpsc, oneshot};
use parking_lot::Mutex;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Io(String),
    /// The adapter answered `success: false`.
    Failed(String),
    /// The connection closed before the answer.
    Closed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) | Error::Failed(e) => f.write_str(e),
            Error::Closed => f.write_str("the debugger closed the connection"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Something the adapter sent unasked.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Event {
        event: String,
        body: Value,
    },
    /// A reverse request (`startDebugging`, `runInTerminal`); answer it with
    /// [`Connection::respond`].
    Request {
        seq: i64,
        command: String,
        arguments: Value,
    },
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>;

pub struct Connection {
    seq: AtomicI64,
    outgoing: std_mpsc::Sender<Vec<u8>>,
    pending: Pending,
    stream: TcpStream,
}

impl Connection {
    pub fn connect(port: u16) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Incoming>)> {
        let stream =
            TcpStream::connect(("127.0.0.1", port)).map_err(|e| Error::Io(e.to_string()))?;
        let _ = stream.set_nodelay(true);
        let reader = stream.try_clone().map_err(|e| Error::Io(e.to_string()))?;
        let writer = stream.try_clone().map_err(|e| Error::Io(e.to_string()))?;
        let (out_tx, out_rx) = std_mpsc::channel::<Vec<u8>>();
        let (in_tx, in_rx) = mpsc::unbounded();
        let pending: Pending = Arc::default();
        std::thread::Builder::new()
            .name("dap-writer".into())
            .spawn(move || write_loop(writer, out_rx))
            .map_err(|e| Error::Io(e.to_string()))?;
        {
            let pending = pending.clone();
            std::thread::Builder::new()
                .name("dap-reader".into())
                .spawn(move || read_loop(reader, pending, in_tx))
                .map_err(|e| Error::Io(e.to_string()))?;
        }
        Ok((
            Arc::new(Self {
                seq: AtomicI64::new(1),
                outgoing: out_tx,
                pending,
                stream,
            }),
            in_rx,
        ))
    }

    fn send(&self, message: Value) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.send_as(seq, message);
    }

    fn send_as(&self, seq: i64, mut message: Value) {
        message["seq"] = seq.into();
        if let Ok(body) = serde_json::to_vec(&message) {
            let _ = self.outgoing.send(body);
        }
    }

    /// Sends a request; the future resolves with the response's `body`.
    pub fn request(
        &self,
        command: &str,
        arguments: Value,
    ) -> impl std::future::Future<Output = Result<Value>> + Send + 'static {
        let (tx, rx) = oneshot::channel();
        // Registered before sending, so a fast answer is never missed.
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.pending.lock().insert(seq, tx);
        self.send_as(
            seq,
            json!({ "type": "request", "command": command, "arguments": arguments }),
        );
        async move { rx.await.unwrap_or(Err(Error::Closed)) }
    }

    pub fn respond(&self, request_seq: i64, command: &str, success: bool, body: Value) {
        self.send(json!({
            "type": "response",
            "request_seq": request_seq,
            "command": command,
            "success": success,
            "body": body,
        }));
    }

    pub fn close(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

fn write_loop(mut stream: TcpStream, rx: std_mpsc::Receiver<Vec<u8>>) {
    while let Ok(body) = rx.recv() {
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        if stream.write_all(header.as_bytes()).is_err()
            || stream.write_all(&body).is_err()
            || stream.flush().is_err()
        {
            break;
        }
    }
}

fn read_loop(stream: TcpStream, pending: Pending, incoming: mpsc::UnboundedSender<Incoming>) {
    let mut reader = BufReader::new(stream);
    while let Some(message) = read_message(&mut reader) {
        let Ok(value) = serde_json::from_slice::<Value>(&message) else {
            continue;
        };
        match value["type"].as_str() {
            Some("response") => {
                let Some(seq) = value["request_seq"].as_i64() else {
                    continue;
                };
                let Some(tx) = pending.lock().remove(&seq) else {
                    continue;
                };
                let result = if value["success"].as_bool().unwrap_or(false) {
                    Ok(value.get("body").cloned().unwrap_or(Value::Null))
                } else {
                    Err(Error::Failed(
                        value["body"]["error"]["format"]
                            .as_str()
                            .or(value["message"].as_str())
                            .unwrap_or("the request failed")
                            .to_string(),
                    ))
                };
                let _ = tx.send(result);
            }
            Some("event") => {
                let _ = incoming.unbounded_send(Incoming::Event {
                    event: value["event"].as_str().unwrap_or_default().to_string(),
                    body: value.get("body").cloned().unwrap_or(Value::Null),
                });
            }
            Some("request") => {
                let _ = incoming.unbounded_send(Incoming::Request {
                    seq: value["seq"].as_i64().unwrap_or(0),
                    command: value["command"].as_str().unwrap_or_default().to_string(),
                    arguments: value.get("arguments").cloned().unwrap_or(Value::Null),
                });
            }
            _ => {}
        }
    }
    for (_, tx) in pending.lock().drain() {
        let _ = tx.send(Err(Error::Closed));
    }
}

/// Reads one `Content-Length`-framed message.
fn read_message(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    Some(body)
}

/// The arguments of `initialize` every session starts with.
pub fn initialize_arguments() -> Value {
    json!({
        "clientID": "solder",
        "clientName": "Solder",
        "adapterID": "pwa-node",
        "pathFormat": "path",
        "linesStartAt1": true,
        "columnsStartAt1": true,
        "supportsVariableType": true,
        "supportsVariablePaging": false,
        "supportsRunInTerminalRequest": false,
        "supportsStartDebuggingRequest": true,
        "locale": "en",
    })
}

/// A debug adapter server process listening on a local port.
pub struct Adapter {
    child: Child,
    pub port: u16,
}

impl Adapter {
    /// Starts `program args... <port> 127.0.0.1` and waits until it listens.
    /// js-debug: `node .../dapDebugServer.js`.
    pub fn start(program: &Path, args: &[String], cwd: &Path) -> Result<Self> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .map_err(|e| Error::Io(e.to_string()))?;
        let mut child = Command::new(program)
            .args(args)
            .arg(port.to_string())
            .arg("127.0.0.1")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Io(format!("{}: {e}", program.display())))?;
        let start = Instant::now();
        loop {
            if TcpStream::connect_timeout(
                &([127, 0, 0, 1], port).into(),
                Duration::from_millis(200),
            )
            .is_ok()
            {
                return Ok(Self { child, port });
            }
            if let Ok(Some(status)) = child.try_wait() {
                let mut err = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut err);
                }
                return Err(Error::Io(format!(
                    "The debug adapter stopped ({status}): {}",
                    err.lines().last().unwrap_or("")
                )));
            }
            if start.elapsed() > Duration::from_secs(15) {
                let _ = child.kill();
                return Err(Error::Io("The debug adapter did not start".into()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        self.stop();
    }
}

// ----------------------------------------------------------------- types

#[derive(Clone, Debug, PartialEq)]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    pub path: Option<PathBuf>,
    /// 1-based.
    pub line: u32,
    pub column: u32,
    /// Library or generated code the user rarely wants to step into.
    pub subtle: bool,
}

pub fn stack_frames(body: &Value) -> Vec<StackFrame> {
    body["stackFrames"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| StackFrame {
            id: f["id"].as_i64().unwrap_or(0),
            name: f["name"].as_str().unwrap_or_default().to_string(),
            path: f["source"]["path"].as_str().map(PathBuf::from),
            line: f["line"].as_u64().unwrap_or(0) as u32,
            column: f["column"].as_u64().unwrap_or(0) as u32,
            subtle: f["presentationHint"] == "subtle"
                || f["source"]["presentationHint"] == "deemphasize",
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub kind: Option<String>,
    /// Non-zero when it has children to fetch with `variables`.
    pub reference: i64,
}

pub fn variables(body: &Value, key: &str) -> Vec<Variable> {
    body[key]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| Variable {
            name: v["name"].as_str().unwrap_or_default().to_string(),
            value: v["value"].as_str().unwrap_or_default().to_string(),
            kind: v["type"].as_str().map(str::to_string),
            reference: v["variablesReference"].as_i64().unwrap_or(0),
        })
        .collect()
}

/// `setBreakpoints` arguments for one file.
pub fn breakpoints_arguments(path: &Path, lines: &[u32]) -> Value {
    json!({
        "source": { "path": path.to_string_lossy(), "name": path.file_name().map(|n| n.to_string_lossy().into_owned()) },
        "breakpoints": lines.iter().map(|l| json!({ "line": l })).collect::<Vec<_>>(),
        "sourceModified": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn frame(value: Value) -> Vec<u8> {
        let body = value.to_string();
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    /// An adapter that answers `initialize`, sends an event and a reverse
    /// request, and fails `evaluate`.
    fn fake_adapter() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut out = stream;
            while let Some(msg) = read_message(&mut reader) {
                let v: Value = serde_json::from_slice(&msg).unwrap();
                match (v["type"].as_str(), v["command"].as_str()) {
                    (Some("request"), Some("initialize")) => {
                        out.write_all(&frame(json!({"seq": 1, "type": "response", "request_seq": v["seq"], "command": "initialize", "success": true, "body": {"supportsConfigurationDoneRequest": true}}))).unwrap();
                        out.write_all(&frame(
                            json!({"seq": 2, "type": "event", "event": "initialized"}),
                        ))
                        .unwrap();
                        out.write_all(&frame(json!({"seq": 3, "type": "request", "command": "startDebugging", "arguments": {"request": "launch", "configuration": {"__pendingTargetId": "t1"}}}))).unwrap();
                    }
                    (Some("request"), Some("evaluate")) => {
                        out.write_all(&frame(json!({"seq": 4, "type": "response", "request_seq": v["seq"], "command": "evaluate", "success": false, "message": "x is not defined"}))).unwrap();
                    }
                    (Some("response"), Some("startDebugging")) => {
                        out.write_all(&frame(json!({"seq": 5, "type": "event", "event": "output", "body": {"output": format!("ack {}", v["request_seq"])}}))).unwrap();
                    }
                    _ => {}
                }
            }
        });
        port
    }

    #[test]
    fn requests_events_and_reverse_requests() {
        let port = fake_adapter();
        let (conn, mut incoming) = Connection::connect(port).unwrap();
        let caps = futures::executor::block_on(conn.request("initialize", initialize_arguments()))
            .unwrap();
        assert_eq!(caps["supportsConfigurationDoneRequest"], true);
        let next = |rx: &mut mpsc::UnboundedReceiver<Incoming>| {
            futures::executor::block_on(futures::StreamExt::next(rx)).unwrap()
        };
        assert_eq!(
            next(&mut incoming),
            Incoming::Event {
                event: "initialized".into(),
                body: Value::Null
            }
        );
        let Incoming::Request {
            seq,
            command,
            arguments,
        } = next(&mut incoming)
        else {
            panic!("a reverse request")
        };
        assert_eq!((seq, command.as_str()), (3, "startDebugging"));
        assert_eq!(arguments["configuration"]["__pendingTargetId"], "t1");
        conn.respond(seq, "startDebugging", true, Value::Null);
        assert_eq!(
            next(&mut incoming),
            Incoming::Event {
                event: "output".into(),
                body: json!({"output": "ack 3"})
            }
        );
        let err = futures::executor::block_on(conn.request("evaluate", json!({"expression": "x"})))
            .unwrap_err();
        assert_eq!(err, Error::Failed("x is not defined".into()));
        conn.close();
    }

    #[test]
    fn reads_stacks_and_variables() {
        let frames = stack_frames(&json!({"stackFrames": [
            {"id": 7, "name": "handler", "line": 12, "column": 5, "source": {"path": "/p/app.js"}},
            {"id": 8, "name": "processTicks", "line": 1, "column": 1, "source": {"name": "<node_internals>", "presentationHint": "deemphasize"}},
        ]}));
        assert_eq!(frames[0].path.as_deref(), Some(Path::new("/p/app.js")));
        assert_eq!((frames[0].line, frames[0].subtle), (12, false));
        assert!(frames[1].subtle && frames[1].path.is_none());
        let vars = variables(
            &json!({"variables": [
                {"name": "user", "value": "{name: 'Ada'}", "type": "object", "variablesReference": 3},
                {"name": "n", "value": "2", "variablesReference": 0},
            ]}),
            "variables",
        );
        assert_eq!(vars[0].reference, 3);
        assert_eq!(vars[1].kind, None);
        let args = breakpoints_arguments(Path::new("/p/app.js"), &[3, 9]);
        assert_eq!(args["breakpoints"][1]["line"], 9);
        assert_eq!(args["source"]["name"], "app.js");
    }

    #[test]
    fn reports_an_adapter_that_does_not_start() {
        let err = Adapter::start(
            Path::new("/bin/sh"),
            &["-c".into(), "echo boom >&2; exit 3".into()],
            Path::new("/"),
        )
        .err()
        .unwrap();
        assert!(matches!(err, Error::Io(e) if e.contains("boom")));
    }
}
