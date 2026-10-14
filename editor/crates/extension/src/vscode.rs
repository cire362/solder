//! The code of a VS Code extension, run in a Node process of its own.
//!
//! A Zed extension is a WebAssembly component in a sandbox. A VS Code
//! extension is a Node program written against VS Code's API, and there is
//! no sandbox for one: it does what the user's account can do. So it runs
//! only once the user allowed it, in a process that holds that extension
//! alone. The process is started with `host/host.js`, which gives the
//! extension a `vscode` module of Solder's own and talks to the editor on
//! its input and output, one JSON message a line.
//!
//! An extension that crashes ends its own process. One that never returns
//! stops answering, and the editor ends the process: nothing else waits on
//! it. Everything here blocks for as long as an answer takes; call it off
//! the UI thread.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use serde_json::{Value, json};

/// The host, kept in the binary: nothing is downloaded to run extensions.
/// The first file is the one Node is started with.
const HOST: &[(&str, &str)] = &[
    ("host.js", include_str!("../host/host.js")),
    ("types.js", include_str!("../host/types.js")),
    ("documents.js", include_str!("../host/documents.js")),
    ("api.js", include_str!("../host/api.js")),
    ("languages.js", include_str!("../host/languages.js")),
    ("debug.js", include_str!("../host/debug.js")),
    ("shell.js", include_str!("../host/shell.js")),
    ("relay.js", include_str!("../host/relay.js")),
    ("views.js", include_str!("../host/views.js")),
    ("webviews.js", include_str!("../host/webviews.js")),
    ("notebooks.js", include_str!("../host/notebooks.js")),
    ("file-save.js", include_str!("../host/file-save.js")),
];

/// Puts the host where Node can read it, under `dir`. A file is written
/// again only when it changed, which is when the editor did.
pub fn host_script(dir: &Path) -> Result<PathBuf, String> {
    for (name, source) in HOST {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).is_ok_and(|there| there == *source) {
            continue;
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(&path, source).map_err(|e| e.to_string())?;
    }
    Ok(dir.join(HOST[0].0))
}

/// What a host tells the editor without being asked.
#[derive(Clone, Debug, PartialEq)]
pub enum Told {
    /// What the extension printed or logged.
    Log { level: String, text: String },
    /// A command it registered, or took back.
    Command { id: String, registered: bool },
    /// A part of VS Code's API it asked for that is not here.
    Missing(String),
    /// Something it asks the editor to do and waits for: to be answered
    /// with [`VsHost::answer`] under the same `id`.
    Asked {
        id: Value,
        method: String,
        params: Value,
    },
    /// Anything else it says and waits for no answer to: what its status
    /// bar item reads now, a line for an output channel.
    Said { method: String, params: Value },
    /// The process ended, with its last words if it had any.
    Gone(String),
}

pub type Answer = Result<Value, String>;
/// Who waits for an answer, by the number of what was asked.
type Waiting = Arc<Mutex<HashMap<u64, Box<dyn FnOnce(Answer) + Send>>>>;

const STOPPED: &str = "The extension's code stopped";

pub struct VsHost {
    child: Mutex<Child>,
    /// Lines on their way to the host. Nothing that sends waits for the
    /// host to read: a thread of its own writes them.
    input: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    waiting: Waiting,
    next: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn write(input: &Mutex<Option<mpsc::Sender<Vec<u8>>>>, message: &Value) -> Result<(), String> {
    let mut line = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    line.push(b'\n');
    lock(input)
        .as_ref()
        .and_then(|input| input.send(line).ok())
        .ok_or_else(|| STOPPED.to_string())
}

/// Writes what is sent to the host, for as long as it reads.
fn feed(mut input: ChildStdin, lines: mpsc::Receiver<Vec<u8>>) {
    for line in lines {
        if input.write_all(&line).and_then(|()| input.flush()).is_err() {
            break;
        }
    }
}

impl VsHost {
    /// Starts the host for the extension in `extension` and waits for it
    /// to say it is there. `node` is the Node to run it with, `script` the
    /// host, and `storage` a folder the extension may keep things in.
    /// `told` hears everything the host says from then on, on a thread of
    /// the host's own.
    pub fn start(
        node: &str,
        script: &Path,
        extension: &Path,
        storage: &Path,
        env: &[(String, String)],
        told: impl Fn(Told) + Send + 'static,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(storage).map_err(|e| e.to_string())?;
        let mut command = Command::new(node);
        command
            .arg(script)
            .arg(extension)
            .arg(storage)
            .current_dir(extension)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !env.is_empty() {
            command.env_clear().envs(env.iter().cloned());
        }
        // A group of its own, so that what the extension starts ends with it.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command
            .spawn()
            .map_err(|e| format!("Could not start Node ({node}): {e}"))?;
        let stdin = child.stdin.take().ok_or("The host has no input")?;
        let (input, lines) = mpsc::channel();
        std::thread::Builder::new()
            .name("vscode-host-input".into())
            .spawn(move || feed(stdin, lines))
            .map_err(|e| e.to_string())?;
        let output = child.stdout.take().ok_or("The host has no output")?;
        let errors = child.stderr.take();
        let waiting: Waiting = Arc::default();
        let (ready, is_ready) = mpsc::channel::<()>();
        let last_words = Arc::new(Mutex::new(String::new()));
        if let Some(errors) = errors {
            let kept = last_words.clone();
            std::thread::Builder::new()
                .name("vscode-host-stderr".into())
                .spawn(move || keep(errors, kept))
                .map_err(|e| e.to_string())?;
        }
        {
            let waiting = waiting.clone();
            let last_words = last_words.clone();
            std::thread::Builder::new()
                .name("vscode-host".into())
                .spawn(move || {
                    read(output, &waiting, &ready, &told);
                    // Whoever still waits learns that no answer will come.
                    let left: Vec<_> = lock(&waiting).drain().collect();
                    for (_, answer) in left {
                        answer(Err(STOPPED.into()));
                    }
                    // Its last words may still be on their way.
                    std::thread::sleep(Duration::from_millis(50));
                    told(Told::Gone(last_line(&lock(&last_words))));
                })
                .map_err(|e| e.to_string())?;
        }
        let host = Self {
            child: Mutex::new(child),
            input: Mutex::new(Some(input)),
            waiting,
            next: AtomicU64::new(1),
        };
        match is_ready.recv_timeout(Duration::from_secs(20)) {
            Ok(()) => Ok(host),
            Err(_) => {
                host.stop();
                std::thread::sleep(Duration::from_millis(100));
                Err(match last_line(&lock(&last_words)) {
                    line if line.is_empty() => "The extension's host did not start".into(),
                    line => format!("The extension's host did not start: {line}"),
                })
            }
        }
    }

    /// Asks the host something without waiting: `answered` hears the
    /// answer on the host's thread, or that the code stopped before it
    /// gave one. An extension that never returns answers nothing until
    /// its process is ended. Gives the number the question went under.
    pub fn ask(
        &self,
        method: &str,
        params: Value,
        answered: impl FnOnce(Answer) + Send + 'static,
    ) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        lock(&self.waiting).insert(id, Box::new(answered));
        let sent = write(
            &self.input,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );
        if let Err(error) = sent
            && let Some(answered) = lock(&self.waiting).remove(&id)
        {
            answered(Err(error));
        }
        id
    }

    /// A native view gave up waiting. Drop its reply closure too; a late
    /// answer has nothing to replace after that view was refreshed.
    pub fn cancel(&self, id: u64) {
        lock(&self.waiting).remove(&id);
    }

    /// Asks the host something and waits for its answer, no longer than
    /// `patience`.
    pub fn request(&self, method: &str, params: Value, patience: Duration) -> Answer {
        let (tx, rx) = mpsc::channel();
        let id = self.ask(method, params, move |answer| {
            let _ = tx.send(answer);
        });
        match rx.recv_timeout(patience) {
            Ok(answer) => answer,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lock(&self.waiting).remove(&id);
                Err(format!(
                    "The extension did not answer in {} s",
                    patience.as_secs().max(1)
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(STOPPED.into()),
        }
    }

    /// Answers something the extension asked ([`Told::Asked`]).
    pub fn answer(&self, id: Value, result: Answer) {
        let message = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32000, "message": error },
            }),
        };
        let _ = write(&self.input, &message);
    }

    /// Tells the host something it does not answer.
    pub fn notify(&self, method: &str, params: Value) {
        let _ = write(
            &self.input,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        );
    }

    /// Whether it still answers: a host whose extension is in a loop that
    /// never ends does not.
    pub fn answers(&self, patience: Duration) -> bool {
        self.request("ping", json!({}), patience).is_ok()
    }

    pub fn is_running(&self) -> bool {
        matches!(lock(&self.child).try_wait(), Ok(None))
    }

    /// Asks it `every` so often whether it still answers, and ends it when
    /// it does not within `patience`: an extension in a loop that never
    /// ends holds its own process, and that only until here. `hung` hears
    /// of it before the process is ended.
    pub fn watch(
        self: &Arc<Self>,
        every: Duration,
        patience: Duration,
        hung: impl FnOnce() + Send + 'static,
    ) {
        let host = Arc::downgrade(self);
        let watching = move || {
            loop {
                std::thread::sleep(every);
                let Some(host) = host.upgrade() else { return };
                if !host.is_running() {
                    return;
                }
                if !host.answers(patience) {
                    if host.is_running() {
                        hung();
                        host.stop();
                    }
                    return;
                }
            }
        };
        let _ = std::thread::Builder::new()
            .name("vscode-host-watch".into())
            .spawn(watching);
    }

    /// Ends the process and what it started.
    pub fn stop(&self) {
        lock(&self.input).take();
        let mut child = lock(&self.child);
        // One that already ended has given its number back, and another
        // program may have it by now: nothing is sent to it.
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        if cfg!(unix) {
            // Its group: an extension may have started programs of its own.
            let _ = Command::new("sh")
                .args(["-c", r#"kill -KILL "-$1" 2>/dev/null"#, "sh"])
                .arg(child.id().to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for VsHost {
    fn drop(&mut self) {
        self.stop();
    }
}

fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn keep(errors: impl Read, kept: Arc<Mutex<String>>) {
    for line in BufReader::new(errors).lines() {
        let Ok(line) = line else { break };
        let mut kept = lock(&kept);
        kept.push_str(&line);
        kept.push('\n');
        if kept.len() > 4000 {
            let mut cut = kept.len() - 4000;
            while !kept.is_char_boundary(cut) {
                cut += 1;
            }
            kept.drain(..cut);
        }
    }
}

/// Reads what the host says until it ends: answers go to whoever waits
/// for them, and the rest to `told`.
fn read(output: impl Read, waiting: &Waiting, ready: &mpsc::Sender<()>, told: &impl Fn(Told)) {
    for line in BufReader::new(output).lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let id = message.get("id").filter(|id| !id.is_null()).cloned();
        let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
        match (id, message["method"].as_str()) {
            (Some(id), None) => {
                let Some(answered) = id.as_u64().and_then(|id| lock(waiting).remove(&id)) else {
                    continue;
                };
                let answer = match message.get("error").filter(|e| !e.is_null()) {
                    Some(error) => Err(text(&error["message"])),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                answered(answer);
            }
            (Some(id), Some(method)) => told(Told::Asked {
                id,
                method: method.to_string(),
                params: message["params"].clone(),
            }),
            (None, Some("ready")) => {
                let _ = ready.send(());
            }
            (None, Some("log")) => told(Told::Log {
                level: text(&message["params"]["level"]),
                text: text(&message["params"]["text"]),
            }),
            (None, Some("command")) => told(Told::Command {
                id: text(&message["params"]["id"]),
                registered: message["params"]["registered"] == true,
            }),
            (None, Some("missing")) => told(Told::Missing(text(&message["params"]["name"]))),
            (None, Some(method)) => told(Told::Said {
                method: method.to_string(),
                params: message["params"].clone(),
            }),
            _ => {}
        }
    }
}

/// Whether an extension that waits for `events` is to be started when
/// `event` happens. `*` waits for nothing; `onLanguage:rust` and the like
/// are compared as written.
pub fn wakes(events: &[String], event: &str) -> bool {
    events.iter().any(|waits| {
        waits == "*" || waits == event || (waits == "onStartupFinished" && event == "*")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{scratch, write as write_file};

    /// An extension in `dir` whose code is `code`, and a host for it. The
    /// test is skipped where there is no Node.
    fn hosted(name: &str, code: &str) -> Option<(VsHost, mpsc::Receiver<Told>, PathBuf)> {
        hosted_logged(name, code).map(|(host, told, dir, _)| (host, told, dir))
    }

    /// What a host said and asked, each as its name and what came with
    /// it, in the order it was said.
    type Log = Arc<Mutex<Vec<(String, Value)>>>;

    /// The same, with everything the host says written down as it is
    /// read: on the thread that reads the host, so that by the time an
    /// answer of the host's reaches whoever asked, what it said before
    /// that answer is in the log.
    fn hosted_logged(
        name: &str,
        code: &str,
    ) -> Option<(VsHost, mpsc::Receiver<Told>, PathBuf, Log)> {
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else {
            eprintln!("skipped: no node to run an extension with");
            return None;
        };
        let dir = scratch(name);
        write_file(
            &dir.join("ext/package.json"),
            r#"{ "name": "demo", "publisher": "Acme", "version": "1.0.0", "main": "./main.js" }"#,
        );
        write_file(&dir.join("ext/main.js"), code);
        let script = host_script(&dir.join("host")).unwrap();
        let (tx, rx) = mpsc::channel();
        let log = Log::default();
        let kept = log.clone();
        let host = VsHost::start(
            &node.to_string_lossy(),
            &script,
            &dir.join("ext"),
            &dir.join("storage"),
            &[],
            move |told| {
                match &told {
                    Told::Asked { method, params, .. } | Told::Said { method, params } => {
                        lock(&kept).push((method.clone(), params.clone()))
                    }
                    Told::Missing(name) => {
                        lock(&kept).push(("missing".into(), json!({ "name": name })))
                    }
                    _ => {}
                }
                let _ = tx.send(told);
            },
        )
        .unwrap();
        Some((host, rx, dir, log))
    }

    const SOON: Duration = Duration::from_secs(20);

    fn heard(told: &mpsc::Receiver<Told>, what: impl Fn(&Told) -> bool) -> Told {
        loop {
            let next = told.recv_timeout(SOON).expect("the host said nothing more");
            if what(&next) {
                return next;
            }
        }
    }

    #[test]
    fn an_extension_is_loaded_given_its_module_and_asked_to_run_a_command() {
        let code = r#"
            const vscode = require('vscode');
            exports.activate = async (context) => {
              console.log('activating', context.extension.id, vscode.env.appName);
              const runs = context.globalState.get('runs', 0) + 1;
              await context.globalState.update('runs', runs);
              context.subscriptions.push(
                vscode.commands.registerCommand('demo.add', (a, b) => ({ sum: a + b, runs })),
                vscode.commands.registerCommand('demo.path', () =>
                  vscode.Uri.joinPath(context.extensionUri, 'media', 'a b.png').toString()),
                vscode.commands.registerCommand('demo.other', () =>
                  vscode.commands.executeCommand('editor.hello', 'x')),
              );
              // A part of the API that is not here does not stop it.
              const tree = vscode.window.createWebviewPanel('demo', 'Demo', 1, {});
              tree.dispose();
              process.stdout.write('printed\n');
              return { api: 1 };
            };
        "#;
        let Some((host, told, dir)) = hosted("vscode-host", code) else {
            return;
        };
        // Nothing of the extension ran yet: it is loaded when asked.
        assert!(host.answers(SOON));
        let activated = host.request("activate", json!({}), SOON).unwrap();
        assert_eq!(
            activated["commands"],
            json!(["demo.add", "demo.path", "demo.other"])
        );
        // What it logs and prints is told, and is not the protocol.
        let said = heard(&told, |told| matches!(told, Told::Log { .. }));
        assert_eq!(
            said,
            Told::Log {
                level: "info".into(),
                text: "activating Acme.demo Solder".into()
            }
        );
        heard(&told, |told| {
            *told
                == Told::Command {
                    id: "demo.add".into(),
                    registered: true,
                }
        });
        heard(
            &told,
            |told| matches!(told, Told::Said { method, .. } if method == "webview"),
        );
        heard(
            &told,
            |told| matches!(told, Told::Log { text, .. } if text == "printed"),
        );

        // Its commands run with what they are given and answer.
        let sum = host.request(
            "executeCommand",
            json!({ "id": "demo.add", "args": [2, 3] }),
            SOON,
        );
        assert_eq!(sum, Ok(json!({ "sum": 5, "runs": 1 })));
        let uri = host
            .request("executeCommand", json!({ "id": "demo.path" }), SOON)
            .unwrap();
        let folder = dir.join("ext").canonicalize().unwrap();
        assert!(uri.as_str().unwrap().starts_with("file://"));
        assert!(uri.as_str().unwrap().ends_with("/ext/media/a%20b.png"));
        assert!(folder.is_dir());
        assert_eq!(
            host.request("executeCommand", json!({ "id": "demo.none" }), SOON),
            Err("No command demo.none".into())
        );

        // A command that is not its own is asked of the editor, and it
        // gets the editor's answer.
        let asking = std::thread::scope(|scope| {
            let running =
                scope.spawn(|| host.request("executeCommand", json!({ "id": "demo.other" }), SOON));
            let Told::Asked { id, method, params } =
                heard(&told, |told| matches!(told, Told::Asked { .. }))
            else {
                unreachable!()
            };
            assert_eq!(method, "executeCommand");
            assert_eq!(params, json!({ "id": "editor.hello", "args": ["x"] }));
            host.answer(id, Ok(json!("hello x")));
            running.join().unwrap()
        });
        assert_eq!(asking, Ok(json!("hello x")));

        // What it keeps is there the next time it runs.
        host.stop();
        assert!(!host.is_running());
        let kept = std::fs::read_to_string(dir.join("storage/global.json")).unwrap();
        assert_eq!(kept, r#"{"runs":1}"#);
    }

    #[test]
    fn an_extension_that_crashes_or_never_returns_holds_nothing_else() {
        // One that throws while it is loaded says so, and its host is
        // still there to be asked.
        let Some((host, told, _)) = hosted("vscode-throws", "throw new Error('no license');")
        else {
            return;
        };
        let error = host.request("activate", json!({}), SOON).unwrap_err();
        assert_eq!(error, "no license");
        assert!(host.answers(SOON));
        drop(told);

        // One that ends its own process: whoever waited is told at once.
        let code =
            "exports.activate = () => { process.stderr.write('giving up\\n'); process.exit(3); };";
        let (host, told, _) = hosted("vscode-exits", code).unwrap();
        let error = host.request("activate", json!({}), SOON).unwrap_err();
        assert_eq!(error, STOPPED);
        assert_eq!(
            heard(&told, |told| matches!(told, Told::Gone(_))),
            Told::Gone("giving up".into())
        );
        assert!(!host.is_running());

        // One whose command never returns: it stops answering, which is
        // how the editor knows to end it. Nothing else waited on it.
        let code = r#"
            const vscode = require('vscode');
            exports.activate = () => {
              vscode.commands.registerCommand('demo.spin', () => { for (;;) {} });
            };
        "#;
        let (host, told, _) = hosted("vscode-spins", code).unwrap();
        host.request("activate", json!({}), SOON).unwrap();
        let brief = Duration::from_millis(500);
        let spun = host.request("executeCommand", json!({ "id": "demo.spin" }), brief);
        assert!(
            spun.unwrap_err()
                .starts_with("The extension did not answer")
        );
        assert!(!host.answers(brief));
        assert!(host.is_running());
        // Watched, it is ended, and what was asked of it is answered: it
        // stopped.
        let host = Arc::new(host);
        let (hung, was_hung) = mpsc::channel();
        let (answered, answer) = mpsc::channel();
        host.ask("ping", json!({}), move |answer| {
            let _ = answered.send(answer);
        });
        host.watch(Duration::from_millis(50), brief, move || {
            let _ = hung.send(());
        });
        was_hung.recv_timeout(SOON).expect("the watch saw nothing");
        assert_eq!(answer.recv_timeout(SOON).unwrap(), Err(STOPPED.to_string()));
        heard(&told, |told| matches!(told, Told::Gone(_)));
        assert!(!host.is_running());

        // One that answers is left alone by its watch.
        let (host, _told, _) = hosted("vscode-fine", "exports.activate = () => {};").unwrap();
        let host = Arc::new(host);
        host.watch(Duration::from_millis(20), SOON, || panic!("it answered"));
        std::thread::sleep(Duration::from_millis(300));
        assert!(host.is_running() && host.answers(SOON));
    }

    /// Plays the editor for a host: answers what it asks with `answer`.
    /// What it says and asks is in `said`, which is given back.
    fn editor(
        host: &Arc<VsHost>,
        told: mpsc::Receiver<Told>,
        said: Log,
        answer: impl Fn(&str, &Value) -> Answer + Send + 'static,
    ) -> Log {
        let host = Arc::downgrade(host);
        std::thread::spawn(move || {
            for told in told {
                if let Told::Asked { id, method, params } = told {
                    let Some(host) = host.upgrade() else { break };
                    host.answer(id, answer(&method, &params));
                }
            }
        });
        said
    }

    /// The lines written to the output channel so far.
    fn output(said: &Mutex<Vec<(String, Value)>>) -> Vec<String> {
        lock(said)
            .iter()
            .filter(|(method, _)| method == "output")
            .filter_map(|(_, params)| params["text"].as_str())
            .map(|line| line.trim_end().to_string())
            .collect()
    }

    fn written(said: &Mutex<Vec<(String, Value)>>, line: &str) {
        let until = std::time::Instant::now() + SOON;
        while !output(said).iter().any(|known| known == line) {
            assert!(
                std::time::Instant::now() < until,
                "no line {line:?} among {:#?}",
                output(said)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn an_extension_has_the_window_the_workspace_and_the_documents() {
        let code = r#"
const vscode = require('vscode');
exports.activate = async (context) => {
  const out = vscode.window.createOutputChannel('Demo');
  const log = (...all) => out.appendLine(all.map((one) => (typeof one === 'string' ? one : JSON.stringify(one))).join(' '));
  const folder = vscode.workspace.workspaceFolders[0];
  log('folder', folder.name, vscode.workspace.name, vscode.workspace.workspaceFolders.length);
  const config = vscode.workspace.getConfiguration('demo');
  log('config', config.get('level'), config.get('none', 'fallback'), config.nested.deep, config.has('level'), vscode.workspace.getConfiguration().get('demo.level'));
  const doc = vscode.workspace.textDocuments[0];
  log('doc', doc.languageId, doc.lineCount, doc.lineAt(1).text, doc.getText(new vscode.Range(0, 3, 0, 7)), doc.offsetAt(new vscode.Position(1, 2)), doc.positionAt(14));
  log('word', doc.getText(doc.getWordRangeAtPosition(new vscode.Position(0, 4))));
  const editor = vscode.window.activeTextEditor;
  log('active', editor.document === doc, editor.selection.active, editor.selection.isReversed);
  vscode.workspace.onDidChangeTextDocument((e) => log('changed', e.document.version, e.contentChanges[0].text, e.contentChanges[0].rangeOffset, e.contentChanges[0].rangeLength, e.document.getText(), e.document.isDirty));
  vscode.workspace.onDidChangeConfiguration((e) => log('configured', e.affectsConfiguration('demo.level'), e.affectsConfiguration('other'), vscode.workspace.getConfiguration('demo').get('level')));
  vscode.workspace.onDidOpenTextDocument((d) => log('opened', vscode.workspace.asRelativePath(d.uri), d.languageId));
  vscode.workspace.onDidCloseTextDocument((d) => log('closed', d.isClosed, vscode.workspace.textDocuments.length));
  vscode.workspace.onDidSaveTextDocument((d) => log('saved', d.isDirty));
  vscode.workspace.onDidChangeWorkspaceFolders((e) => log('folders', e.added.length, e.removed.length));
  vscode.window.onDidChangeTextEditorSelection((e) => log('selected', e.selections[0].active.line));
  vscode.window.onDidChangeActiveTextEditor((e) => log('front', e ? vscode.workspace.asRelativePath(e.document.uri) : 'none'));
  vscode.window.onDidChangeActiveColorTheme((theme) => log('theme', theme.kind));
  const item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 5);
  item.text = '$(check) Demo';
  item.command = 'demo.ask';
  item.show();
  context.subscriptions.push(vscode.commands.registerCommand('demo.ask', async () => {
    const answer = await vscode.window.showInformationMessage('Go on?', { modal: true }, 'Yes', 'No');
    const picked = await vscode.window.showQuickPick([{ label: 'one', description: 'first' }, { label: 'two' }], { placeHolder: 'Which' });
    const name = await vscode.window.showInputBox({ prompt: 'Name', validateInput: (value) => (value.length < 3 ? 'Too short' : undefined) });
    const applied = await vscode.window.activeTextEditor.edit((builder) => builder.insert(new vscode.Position(0, 0), '// '));
    const edit = new vscode.WorkspaceEdit();
    edit.replace(doc.uri, new vscode.Range(1, 0, 1, 1), 'X');
    const also = await vscode.workspace.applyEdit(edit);
    await vscode.workspace.getConfiguration('demo').update('level', 9);
    await vscode.env.clipboard.writeText('copied');
    const none = await vscode.window.showQuickPick(['a']);
    return { answer, picked: picked && picked.label, name, applied, also, none: none === undefined };
  }));
  const found = await vscode.workspace.findFiles('**/*.{rs,md}', '**/skip/**');
  log('found', found.map((uri) => vscode.workspace.asRelativePath(uri)).sort());
  const bytes = await vscode.workspace.fs.readFile(vscode.Uri.joinPath(folder.uri, 'a.rs'));
  log('read', Buffer.from(bytes).toString().trim());
  const disk = await vscode.workspace.openTextDocument(vscode.Uri.joinPath(folder.uri, 'docs/b.md'));
  log('disk', disk.languageId, disk.lineCount, (await vscode.workspace.openTextDocument(doc.uri)) === doc);
  await vscode.window.withProgress({ title: 'Working' }, async (progress) => progress.report({ message: 'half' }));
  vscode.window.showWarningMessage('Careful');
  log(vscode.l10n.t('Hello {0}', 'you'), new vscode.Range(2, 0, 1, 0).start.line, vscode.ViewColumn.Two);
  // What is not here yet does not break it.
  const watcher = vscode.workspace.createFileSystemWatcher('**/*');
  watcher.onDidChange(() => {});
  class Item extends vscode.TreeItem {}
  new Item('x');
  log('ready');
};
"#;
        let Some((host, told, dir, log)) = hosted_logged("vscode-api", code) else {
            return;
        };
        let project = dir.join("project").canonicalize().unwrap_or_else(|_| {
            std::fs::create_dir_all(dir.join("project")).unwrap();
            dir.join("project").canonicalize().unwrap()
        });
        write_file(&project.join("a.rs"), "fn main() {}\n");
        write_file(&project.join("docs/b.md"), "# B\n\ntext\n");
        write_file(&project.join("skip/c.rs"), "");
        write_file(&project.join("node_modules/d/e.rs"), "");
        write_file(&project.join("f.txt"), "");
        let uri = |name: &str| format!("file://{}/{name}", project.display());
        let host = Arc::new(host);
        let typed = Arc::new(Mutex::new(vec!["ab", "abc"]));
        let said = editor(&host, told, log, move |method, params| match method {
            "message" => Ok(json!(0)),
            "pick" if params["placeholder"] == "Which" => Ok(json!(1)),
            "pick" => Ok(Value::Null),
            "input" => Ok(json!(lock(&typed).remove(0))),
            "applyEdit" | "updateConfiguration" | "clipboardWrite" => Ok(json!(true)),
            other => Err(format!("no {other} here")),
        });
        // Everything the editor has is said before the extension is loaded.
        host.notify(
            "init",
            json!({
                "folders": [project],
                "configuration": { "demo": { "level": 2, "nested": { "deep": "yes" } } },
                "dark": true,
                "documents": [
                    { "uri": uri("a.rs"), "languageId": "rust", "version": 1, "text": "fn main() {}\nlet a = 1;\n" },
                ],
                "active": { "uri": uri("a.rs"), "selections": [
                    { "anchor": { "line": 1, "character": 4 }, "active": { "line": 0, "character": 2 } },
                ] },
            }),
        );
        host.request("activate", json!({}), SOON).unwrap();
        written(&said, "ready");
        let lines = output(&said);
        assert_eq!(
            lines[..10],
            [
                "folder project project 1",
                "config 2 fallback yes true 2",
                r#"doc rust 3 let a = 1; main 15 {"line":1,"character":1}"#,
                "word main",
                r#"active true {"line":0,"character":2} true"#,
                r#"found ["a.rs","docs/b.md"]"#,
                "read fn main() {}",
                "disk markdown 4 true",
                "Hello you 1 2",
                "ready",
            ]
        );
        // What it showed: an item of the status bar, a message that waits
        // for nothing, and work in progress that began and ended.
        let sent = |method: &str| -> Vec<Value> {
            lock(&said)
                .iter()
                .filter(|(known, _)| known == method)
                .map(|(_, params)| params.clone())
                .collect()
        };
        assert_eq!(
            sent("status"),
            [json!({
                "id": 1, "text": "$(check) Demo", "command": "demo.ask", "visible": true,
                "right": true, "priority": 5,
            })]
        );
        assert_eq!(sent("message")[0]["text"], "Careful");
        assert_eq!(sent("message")[0]["level"], "warning");
        assert_eq!(
            sent("progress"),
            [
                json!({ "id": 1, "text": "Working" }),
                json!({ "id": 1, "text": "Working: half" }),
                json!({ "id": 1, "done": true }),
            ]
        );
        // The editor says what changes, and the extension's copy follows.
        let change = |version: u64, line: u64, from: u64, to: u64, text: &str| {
            json!({ "uri": uri("a.rs"), "version": version, "changes": [{
                "range": { "start": { "line": line, "character": from }, "end": { "line": line, "character": to } },
                "text": text,
            }] })
        };
        host.notify("changed", change(2, 1, 4, 5, "bc"));
        written(&said, "changed 2 bc 17 1 fn main() {}\nlet bc = 1;\n true");
        host.notify("saved", json!({ "uri": uri("a.rs") }));
        written(&said, "saved false");
        host.notify(
            "active",
            json!({ "uri": uri("a.rs"), "selections": [
                { "anchor": { "line": 1, "character": 0 }, "active": { "line": 1, "character": 0 } },
            ] }),
        );
        written(&said, "selected 1");
        host.notify(
            "opened",
            json!({ "uri": uri("docs/b.md"), "languageId": "markdown", "version": 1, "text": "# B\n" }),
        );
        written(&said, "opened docs/b.md markdown");
        host.notify("active", json!({ "uri": uri("docs/b.md") }));
        written(&said, "front docs/b.md");
        host.notify("closed", json!({ "uri": uri("docs/b.md") }));
        written(&said, "front none");
        written(&said, "closed true 1");
        host.notify(
            "configuration",
            json!({ "configuration": { "demo": { "level": 3 } } }),
        );
        written(&said, "configured true false 3");
        host.notify("folders", json!({ "folders": [project, dir] }));
        written(&said, "folders 1 0");
        host.notify("theme", json!({ "dark": false }));
        written(&said, "theme 1");

        // Its command asks the user three things and changes the text two
        // ways; what is typed is checked by the extension and asked again.
        host.notify("active", json!({ "uri": uri("a.rs") }));
        written(&said, "front a.rs");
        let ran = host.request("executeCommand", json!({ "id": "demo.ask" }), SOON);
        assert_eq!(
            ran,
            Ok(json!({
                "answer": "Yes", "picked": "two", "name": "abc", "applied": true, "also": true,
                "none": true,
            }))
        );
        let asked = sent("message");
        assert_eq!(
            asked[1],
            json!({ "level": "info", "text": "Go on?", "modal": true, "items": ["Yes", "No"] })
        );
        assert_eq!(
            sent("pick")[0],
            json!({ "placeholder": "Which", "items": [
                { "label": "one", "description": "first" }, { "label": "two" },
            ] })
        );
        let inputs = sent("input");
        assert_eq!(inputs[0]["prompt"], "Name");
        assert_eq!(
            (&inputs[1]["prompt"], &inputs[1]["value"]),
            (&json!("Too short"), &json!("ab"))
        );
        let edits = sent("applyEdit");
        assert_eq!(
            edits[0]["changes"][uri("a.rs")],
            json!([{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }, "newText": "// " }])
        );
        assert_eq!(edits[1]["changes"][uri("a.rs")][0]["newText"], "X");
        assert_eq!(
            sent("updateConfiguration"),
            [json!({ "key": "demo.level", "value": 9 })]
        );
        assert_eq!(sent("clipboardWrite"), [json!({ "text": "copied" })]);
    }

    #[test]
    fn a_host_answers_as_a_language_server() {
        let code = r#"
const vscode = require('vscode');
exports.activate = (context) => {
  const problems = vscode.languages.createDiagnosticCollection('demo');
  const legend = new vscode.SemanticTokensLegend(['keyword', 'function', 'variable'], ['readonly']);
  context.subscriptions.push(
    vscode.languages.registerReferenceProvider('rust', {
      provideReferences: (document, position, context) => [
        new vscode.Location(document.uri, new vscode.Range(0, 3, 0, 7)),
        ...(context.includeDeclaration ? [new vscode.Location(document.uri, new vscode.Position(1, 0))] : []),
      ],
    }),
    vscode.languages.registerRenameProvider({ language: 'rust' }, {
      prepareRename: (document, position) => document.getWordRangeAtPosition(position),
      provideRenameEdits: () => undefined,
    }),
    vscode.languages.registerWorkspaceSymbolProvider({
      provideWorkspaceSymbols: (query) => [
        new vscode.SymbolInformation(query + '_found', vscode.SymbolKind.Struct, 'mod', new vscode.Location(vscode.Uri.file('/tmp/a.rs'), new vscode.Range(2, 0, 2, 5))),
      ],
    }),
    vscode.languages.registerHoverProvider(['rust', 'go'], {
      provideHover: () => new vscode.Hover(['plain', { language: 'rust', value: 'fn main()' }]),
    }),
    vscode.languages.registerCompletionItemProvider('go', { provideCompletionItems: () => [] }, '.', ':'),
    vscode.languages.registerInlayHintsProvider('rust', {
      provideInlayHints(document, range) {
        const typed = new vscode.InlayHint(new vscode.Position(1, 5), [new vscode.InlayHintLabelPart(': '), new vscode.InlayHintLabelPart('i32')], vscode.InlayHintKind.Type);
        typed.paddingRight = true;
        return [typed, new vscode.InlayHint(range.end, 'end')];
      },
    }),
    vscode.languages.registerDocumentSemanticTokensProvider('rust', {
      provideDocumentSemanticTokens(document) {
        const builder = new vscode.SemanticTokensBuilder(legend);
        // In any order: the builder sorts them.
        builder.push(new vscode.Range(1, 4, 1, 5), 'variable', ['readonly']);
        builder.push(new vscode.Range(0, 3, 0, 7), 'function');
        builder.push(0, 0, 2, 0, 0);
        return builder.build();
      },
    }, legend),
    vscode.languages.registerImplementationProvider('rust', {
      provideImplementation: (document) => new vscode.Location(document.uri, new vscode.Range(1, 4, 1, 5)),
    }),
    vscode.languages.registerDocumentHighlightProvider('rust', {
      provideDocumentHighlights: () => [
        new vscode.DocumentHighlight(new vscode.Range(0, 3, 0, 7), vscode.DocumentHighlightKind.Write),
        new vscode.DocumentHighlight(new vscode.Range(1, 4, 1, 5)),
      ],
    }),
    vscode.languages.registerCodeLensProvider('rust', {
      provideCodeLenses: () => [
        new vscode.CodeLens(new vscode.Range(0, 0, 0, 2), { title: 'Mark', command: 'demo.mark' }),
        new vscode.CodeLens(new vscode.Range(1, 0, 1, 3)),
      ],
      // What the second one does is said only when it is asked.
      resolveCodeLens(lens) {
        lens.command = { title: 'Late', command: 'demo.late', arguments: [lens.range.start.line] };
        return lens;
      },
    }),
    vscode.commands.registerCommand('demo.late', (line) => `late ${line}`),
    vscode.commands.registerCommand('demo.mark', (uri) => {
      problems.set(uri, [new vscode.Diagnostic(new vscode.Range(0, 0, 0, 2), 'marked', vscode.DiagnosticSeverity.Hint)]);
      return vscode.languages.getDiagnostics(uri).length;
    }),
  );
  // What VS Code has and Solder does not is taken without a word of
  // complaint, and said to the editor.
  vscode.languages.registerFoldingRangeProvider('rust', {});
};
"#;
        let Some((host, told, dir, log)) = hosted_logged("vscode-lsp", code) else {
            return;
        };
        let host = Arc::new(host);
        let said = editor(&host, told, log, |method, _| {
            Err(format!("no {method} here"))
        });
        let file = format!("file://{}/a.rs", dir.display());
        host.notify(
            "init",
            json!({ "documents": [
                { "uri": file, "languageId": "rust", "version": 1, "text": "fn main() {}\nlet a = 1;\n" },
            ] }),
        );
        host.request("activate", json!({}), SOON).unwrap();
        // Asks the server the host is, and gives its answer.
        let ask = |id: u64, method: &str, params: Value| -> Value {
            let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
            host.notify("lsp", json!({ "message": message }));
            let until = std::time::Instant::now() + SOON;
            loop {
                let answer = lock(&said).iter().find_map(|(method, params)| {
                    (method == "lsp" && params["message"]["id"] == id)
                        .then(|| params["message"].clone())
                });
                if let Some(answer) = answer {
                    return answer;
                }
                assert!(std::time::Instant::now() < until, "no answer to {method}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let at = |line: u64, character: u64| json!({ "textDocument": { "uri": file }, "position": { "line": line, "character": character } });

        // It says for which languages it has something, and what.
        let until = std::time::Instant::now() + SOON;
        let languages = loop {
            let last = lock(&said)
                .iter()
                .rev()
                .find(|(method, _)| method == "providers")
                .map(|(_, params)| params["languages"].clone());
            if let Some(languages) = last {
                break languages;
            }
            assert!(std::time::Instant::now() < until, "no providers said");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(languages, json!(["rust", "*", "go"]));
        let can = ask(1, "initialize", json!({}))["result"]["capabilities"].clone();
        assert_eq!(can["referencesProvider"], true);
        assert_eq!(can["renameProvider"], json!({ "prepareProvider": true }));
        assert_eq!(can["workspaceSymbolProvider"], true);
        assert_eq!(
            can["completionProvider"],
            json!({ "triggerCharacters": [".", ":"] })
        );
        assert_eq!(
            can["textDocumentSync"],
            json!({ "openClose": true, "change": 0 })
        );
        assert!(can["definitionProvider"].is_null() && can["documentFormattingProvider"].is_null());
        assert_eq!(can["codeLensProvider"], json!({ "resolveProvider": true }));
        assert_eq!(
            can["executeCommandProvider"],
            json!({ "commands": ["solder.run", "solder.lens"] })
        );
        assert_eq!(can["inlayHintProvider"], true);
        assert_eq!(
            can["semanticTokensProvider"],
            json!({ "full": true, "legend": {
                "tokenTypes": ["keyword", "function", "variable"], "tokenModifiers": ["readonly"],
            } })
        );

        let mut references = at(0, 4);
        references["context"] = json!({ "includeDeclaration": true });
        let range = |a: u64, b: u64, c: u64, d: u64| json!({ "start": { "line": a, "character": b }, "end": { "line": c, "character": d } });
        assert_eq!(
            ask(2, "textDocument/references", references)["result"],
            json!([
                { "uri": file, "range": range(0, 3, 0, 7) },
                { "uri": file, "range": range(1, 0, 1, 0) },
            ])
        );
        assert_eq!(
            ask(3, "textDocument/prepareRename", at(0, 4))["result"],
            range(0, 3, 0, 7)
        );
        assert_eq!(
            ask(4, "workspace/symbol", json!({ "query": "ma" }))["result"],
            json!([{
                "name": "ma_found", "kind": 23, "containerName": "mod",
                "location": { "uri": "file:///tmp/a.rs", "range": range(2, 0, 2, 5) },
            }])
        );
        assert_eq!(
            ask(5, "textDocument/hover", at(0, 4))["result"]["contents"],
            json!({ "kind": "markdown", "value": "plain\n\n```rust\nfn main()\n```" })
        );
        // What it draws into the text: hints, and what each word is as the
        // run of numbers the protocol has for it.
        let whole = json!({ "textDocument": { "uri": file }, "range": range(0, 0, 2, 0) });
        assert_eq!(
            ask(10, "textDocument/inlayHint", whole)["result"],
            json!([
                { "position": { "line": 1, "character": 5 }, "kind": 1, "paddingRight": true,
                  "label": [{ "value": ": " }, { "value": "i32" }] },
                { "position": { "line": 2, "character": 0 }, "label": "end" },
            ])
        );
        assert_eq!(
            ask(
                11,
                "textDocument/semanticTokens/full",
                json!({ "textDocument": { "uri": file } })
            )["result"],
            json!({ "data": [0, 0, 2, 0, 0, 0, 3, 4, 1, 0, 1, 4, 1, 2, 1] })
        );
        // What it offers to do with a line: the command is one of the
        // host's own, which names the lens by its file and its place, so
        // that a lens left in its line for an hour still runs what it said.
        let lenses = ask(
            12,
            "textDocument/codeLens",
            json!({ "textDocument": { "uri": file } }),
        )["result"]
            .clone();
        assert_eq!(
            lenses,
            json!([
                { "range": range(0, 0, 0, 2), "data": { "uri": file, "nth": 0 },
                  "command": { "title": "Mark", "command": "solder.lens", "arguments": [file, 0] } },
                { "range": range(1, 0, 1, 3), "data": { "uri": file, "nth": 1 } },
            ])
        );
        let late = json!({ "title": "Late", "command": "solder.lens", "arguments": [file, 1] });
        assert_eq!(
            ask(13, "codeLens/resolve", lenses[1].clone())["result"]["command"],
            late
        );
        let run = |id: u64, nth: u64| {
            let params = json!({ "command": "solder.lens", "arguments": [file, nth] });
            ask(id, "workspace/executeCommand", params)
        };
        assert_eq!(run(14, 1)["result"], "late 1");
        assert_eq!(run(15, 7)["error"]["code"], -32603);
        // The places that implement a symbol, and the other places it is
        // used: the kinds of those are counted from one in the protocol.
        assert_eq!(can["implementationProvider"], true);
        assert_eq!(can["documentHighlightProvider"], true);
        assert_eq!(
            ask(16, "textDocument/implementation", at(0, 4))["result"],
            json!([{ "uri": file, "range": range(1, 4, 1, 5) }])
        );
        assert_eq!(
            ask(17, "textDocument/documentHighlight", at(0, 4))["result"],
            json!([
                { "range": range(0, 3, 0, 7), "kind": 3 },
                { "range": range(1, 4, 1, 5), "kind": 1 },
            ])
        );
        // What it has no provider for, and a file it was not told of.
        assert!(
            ask(6, "textDocument/definition", at(0, 4))["result"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let elsewhere = json!({ "textDocument": { "uri": "file:///nowhere.rs" }, "position": { "line": 0, "character": 0 } });
        assert!(ask(7, "textDocument/hover", elsewhere)["result"].is_null());
        assert_eq!(
            ask(8, "textDocument/foldingRange", at(0, 0))["error"]["code"],
            -32601
        );

        // What it reports goes out as a server's diagnostics do, and again
        // to an editor that begins to listen.
        let published = || -> Vec<Value> {
            lock(&said)
                .iter()
                .filter(|(method, params)| {
                    method == "lsp"
                        && params["message"]["method"] == "textDocument/publishDiagnostics"
                })
                .map(|(_, params)| params["message"]["params"].clone())
                .collect()
        };
        assert!(published().is_empty());
        // A file given to a command arrives as the Uri it was.
        let marked = host.request(
            "executeCommand",
            json!({ "id": "demo.mark", "args": [{ "$uri": file }] }),
            SOON,
        );
        assert_eq!(marked, Ok(json!(1)));
        let one = json!({ "uri": file, "diagnostics": [{
            "range": range(0, 0, 0, 2), "message": "marked", "severity": 4,
        }] });
        assert_eq!(published(), std::slice::from_ref(&one));
        host.notify(
            "lsp",
            json!({ "message": { "jsonrpc": "2.0", "method": "initialized", "params": {} } }),
        );
        ask(9, "shutdown", Value::Null);
        assert_eq!(published(), [one.clone(), one]);
        assert!(lock(&said).iter().any(|(method, params)| {
            method == "missing" && params["name"] == "languages.registerFoldingRangeProvider"
        }));
    }

    #[test]
    fn an_extension_has_terminals_tasks_and_hears_of_files_that_change() {
        let code = r#"
const vscode = require('vscode');
exports.activate = (context) => {
  const out = vscode.window.createOutputChannel('Shell');
  const log = (...all) => out.appendLine(all.map((one) => (typeof one === 'string' ? one : JSON.stringify(one))).join(' '));
  const name = (uri) => vscode.workspace.asRelativePath(uri);
  const watcher = vscode.workspace.createFileSystemWatcher('**/*.txt');
  watcher.onDidCreate((uri) => log('created', name(uri)));
  watcher.onDidChange((uri) => log('changed', name(uri)));
  watcher.onDidDelete((uri) => log('deleted', name(uri)));
  const terminal = vscode.window.createTerminal({ name: 'Demo', shellPath: '/bin/sh', cwd: vscode.workspace.workspaceFolders[0].uri });
  vscode.window.onDidCloseTerminal((closed) => log('closed', closed.name, closed.exitStatus.code, vscode.window.terminals.length));
  vscode.tasks.registerTaskProvider('demo', {
    provideTasks: () => [
      new vscode.Task({ type: 'demo' }, vscode.TaskScope.Workspace, 'build', 'demo', new vscode.ShellExecution('echo', ['a b', 'c'])),
      new vscode.Task({ type: 'demo' }, vscode.TaskScope.Workspace, 'list', 'demo', new vscode.ProcessExecution('ls', ['-la'], { cwd: '/tmp', env: { A: '1' } })),
      new vscode.Task({ type: 'demo' }, 'line', 'demo', new vscode.ShellExecution('make all')),
    ],
  });
  vscode.tasks.onDidStartTask((e) => log('task started', e.execution.task.name));
  vscode.tasks.onDidEndTaskProcess((e) => log('task ended', e.execution.task.name, e.exitCode, vscode.tasks.taskExecutions.length));
  context.subscriptions.push(watcher, vscode.commands.registerCommand('demo.type', () => {
    terminal.sendText('echo hi');
    terminal.show();
    return vscode.window.terminals.length;
  }));
  log('ready');
};
"#;
        let Some((host, told, dir, log)) = hosted_logged("vscode-shell", code) else {
            return;
        };
        let project = dir.join("project");
        write_file(&project.join("old.txt"), "old");
        let project = project.canonicalize().unwrap();
        let host = Arc::new(host);
        let said = editor(&host, told, log, |method, _| {
            Err(format!("no {method} here"))
        });
        host.notify("init", json!({ "folders": [project] }));
        host.request("activate", json!({}), SOON).unwrap();
        written(&said, "ready");
        let sent = |method: &str| -> Vec<Value> {
            lock(&said)
                .iter()
                .filter(|(known, _)| known == method)
                .map(|(_, params)| params.clone())
                .collect()
        };

        // A terminal it only keeps ready is none in the editor. Typed
        // into, it is made, with what it runs and where.
        assert!(sent("terminal.create").is_empty());
        let count = host.request("executeCommand", json!({ "id": "demo.type" }), SOON);
        assert_eq!(count, Ok(json!(1)));
        assert_eq!(
            sent("terminal.create"),
            [json!({
                "id": 1, "name": "Demo", "program": "/bin/sh", "args": [],
                "cwd": project, "keep": false, "show": false,
            })]
        );
        assert_eq!(
            sent("terminal.send"),
            [json!({ "id": 1, "text": "echo hi\n" })]
        );
        assert_eq!(sent("terminal.show"), [json!({ "id": 1 })]);
        host.notify("terminal.closed", json!({ "id": 1, "code": 0 }));
        written(&said, "closed Demo 0 0");

        // Its tasks, and one of them run: a command line for the shell,
        // each word quoted where the shell would part it.
        let tasks = host.request("tasks.fetch", json!({}), SOON).unwrap();
        assert_eq!(
            tasks,
            json!([
                { "name": "build", "source": "demo" },
                { "name": "list", "source": "demo" },
                { "name": "line", "source": "demo" },
            ])
        );
        host.request("tasks.run", json!({ "index": 0 }), SOON)
            .unwrap();
        written(&said, "task started build");
        let made = sent("terminal.create");
        assert_eq!(made[1]["name"], "build");
        assert_eq!(made[1]["args"], json!(["-c", "echo 'a b' c"]));
        assert_eq!(
            (&made[1]["keep"], &made[1]["show"]),
            (&json!(true), &json!(true))
        );
        // It ends when what ran in its terminal does, with what that
        // ended with.
        host.notify("terminal.closed", json!({ "id": 2, "code": 3 }));
        written(&said, "task ended build 3 0");
        // A program with what it is given, and a line as it was written.
        host.request("tasks.run", json!({ "index": 1 }), SOON)
            .unwrap();
        host.request("tasks.run", json!({ "index": 2 }), SOON)
            .unwrap();
        let made = sent("terminal.create");
        assert_eq!(
            (
                &made[2]["program"],
                &made[2]["args"],
                &made[2]["cwd"],
                &made[2]["env"]
            ),
            (
                &json!("ls"),
                &json!(["-la"]),
                &json!("/tmp"),
                &json!({ "A": "1" })
            )
        );
        assert_eq!(made[3]["args"], json!(["-c", "make all"]));
        assert!(
            host.request("tasks.run", json!({ "index": 9 }), SOON)
                .is_err()
        );

        // Files that change on disk: a new one, a changed one, one in a
        // folder made after the watching began, and one that went. A file
        // that was there is changed, not new; one of another ending is
        // none of its business.
        std::fs::write(project.join("a.txt"), "a").unwrap();
        written(&said, "created a.txt");
        std::fs::write(project.join("old.txt"), "older").unwrap();
        written(&said, "changed old.txt");
        std::fs::write(project.join("c.md"), "c").unwrap();
        write_file(&project.join("sub/deep/b.txt"), "b");
        written(&said, "created sub/deep/b.txt");
        std::fs::remove_file(project.join("a.txt")).unwrap();
        written(&said, "deleted a.txt");
        assert!(!output(&said).iter().any(|line| line.contains("c.md")));
    }

    #[test]
    fn a_terminal_and_a_task_an_extension_draws_itself_run_in_a_terminal() {
        use std::io::{Read, Write};
        let code = r#"
const vscode = require('vscode');
// A terminal that is an object here: it says its size, shouts back what is
// typed, and on Enter ends with what it was told to end with.
function shouting(ending, said) {
  const write = new vscode.EventEmitter();
  const close = new vscode.EventEmitter();
  return {
    onDidWrite: write.event,
    onDidClose: close.event,
    open: (size) => write.fire(`open ${size.columns}x${size.rows}\r\n`),
    close: () => said('let go'),
    // Keys come one at a time from a keyboard and several at once from
    // whatever pastes: each is looked at on its own.
    handleInput(data) {
      for (const key of data) {
        if (key === '\r') {
          write.fire('done\r\n');
          close.fire(ending);
        } else {
          write.fire(key.toUpperCase());
        }
      }
    },
  };
}
exports.activate = (context) => {
  const out = vscode.window.createOutputChannel('Pty');
  const log = (...all) => out.appendLine(all.join(' '));
  vscode.window.onDidCloseTerminal((closed) => log('closed', closed.name, closed.exitStatus.code));
  vscode.tasks.onDidEndTaskProcess((e) => log('task ended', e.execution.task.name, e.exitCode));
  vscode.tasks.registerTaskProvider('demo', {
    provideTasks: () => [new vscode.Task({ type: 'demo' }, vscode.TaskScope.Workspace, 'own', 'demo',
      new vscode.CustomExecution(async () => shouting(0, log)))],
  });
  context.subscriptions.push(vscode.commands.registerCommand('demo.pty', () => {
    const terminal = vscode.window.createTerminal({ name: 'Mine', pty: shouting(3, log) });
    terminal.show();
  }));
  log('ready');
};
"#;
        let Some((host, told, _dir, log)) = hosted_logged("vscode-pty", code) else {
            return;
        };
        let host = Arc::new(host);
        let said = editor(&host, told, log, |method, _| {
            Err(format!("no {method} here"))
        });
        host.notify("init", json!({}));
        host.request("activate", json!({}), SOON).unwrap();
        written(&said, "ready");
        // What the dock's terminal is told to run, once it is said.
        let made = |count: usize| -> Value {
            let until = std::time::Instant::now() + SOON;
            loop {
                let all: Vec<Value> = lock(&said)
                    .iter()
                    .filter(|(method, _)| method == "terminal.create")
                    .map(|(_, params)| params.clone())
                    .collect();
                if all.len() >= count {
                    return all[count - 1].clone();
                }
                assert!(std::time::Instant::now() < until, "no terminal was made");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        // Plays the terminal: runs what it was told to, types `typed`, and
        // gives what was shown and what the program ended with.
        let terminal = |create: &Value, typed: &str| -> (String, Option<i32>) {
            let args: Vec<&str> = create["args"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let mut child = Command::new(create["program"].as_str().unwrap())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(typed.as_bytes())
                .unwrap();
            let mut shown = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut shown)
                .unwrap();
            (shown, child.wait().unwrap().code())
        };

        // A terminal the extension draws is a terminal of the dock that
        // runs the relay. What is typed reaches the extension's object,
        // what it writes is shown, and what it ends with is what the
        // program in the terminal ends with.
        host.request("executeCommand", json!({ "id": "demo.pty" }), SOON)
            .unwrap();
        let create = made(1);
        assert_eq!(
            (&create["name"], &create["show"]),
            (&json!("Mine"), &json!(true))
        );
        assert!(create["args"][0].as_str().unwrap().ends_with("relay.js"));
        let (shown, code) = terminal(&create, "hi\r");
        assert_eq!(shown, "open 80x24\r\nHIdone\r\n");
        assert_eq!(code, Some(3));
        // Its terminal gone, the object is let go; and the extension hears
        // of the end as of any terminal's.
        written(&said, "let go");
        host.notify("terminal.closed", json!({ "id": create["id"], "code": 3 }));
        written(&said, "closed Mine 3");

        // The port takes its own terminal and nobody else: without the
        // token nothing is shown, and the one with it has gone already.
        let mut stranger = create.clone();
        stranger["args"][2] = json!("0000");
        let (shown, _) = terminal(&stranger, "x");
        assert_eq!(shown, "");

        // A task that is the extension's own code runs the same way, in a
        // terminal that keeps its tab, and ends when its terminal does.
        let tasks = host.request("tasks.fetch", json!({}), SOON).unwrap();
        assert_eq!(tasks[0]["name"], "own");
        host.request("tasks.run", json!({ "index": 0 }), SOON)
            .unwrap();
        let create = made(2);
        assert_eq!(
            (&create["name"], &create["keep"]),
            (&json!("own"), &json!(true))
        );
        let (shown, code) = terminal(&create, "go\r");
        assert!(shown.ends_with("GOdone\r\n"), "{shown:?}");
        assert_eq!(code, Some(0));
        host.notify("terminal.closed", json!({ "id": create["id"], "code": 0 }));
        written(&said, "task ended own 0");
    }

    #[test]
    fn an_extension_has_pages_views_that_are_pages_and_editors_of_its_own() {
        let code = r#"
const vscode = require('vscode');
exports.activate = (context) => {
  const out = vscode.window.createOutputChannel('Pages');
  const log = (...all) => out.appendLine(all.map((one) => (typeof one === 'string' ? one : JSON.stringify(one))).join(' '));
  let panel;
  context.subscriptions.push(
    vscode.window.registerWebviewPanelSerializer('demo.saved', {
      async deserializeWebviewPanel(page, state) {
        page.webview.html = `<p>restored ${state.count}</p>`;
        log('restored', state);
      },
    }),
    vscode.commands.registerCommand('demo.page', () => {
      panel = vscode.window.createWebviewPanel('demo.page', 'Demo page', vscode.ViewColumn.Two,
        { enableScripts: true, localResourceRoots: [context.extensionUri] });
      panel.webview.html = '<p>hello</p>';
      panel.webview.onDidReceiveMessage((message) => log('got', message));
      panel.onDidChangeViewState((e) => log('state', e.webviewPanel.visible, e.webviewPanel.active));
      panel.onDidDispose(() => log('disposed'));
      const picture = panel.webview.asWebviewUri(vscode.Uri.joinPath(context.extensionUri, 'media', 'a b.png'));
      log('uri', picture.toString().endsWith('/ext/media/a%20b.png'), picture.authority, panel.webview.cspSource);
    }),
    vscode.commands.registerCommand('demo.post', () => panel.webview.postMessage({ n: 1 })),
    vscode.window.registerWebviewViewProvider('demo.side', {
      resolveWebviewView(view) {
        view.webview.html = '<b>side</b>';
        log('resolved', view.viewType, view.title);
      },
    }),
    vscode.window.registerCustomEditorProvider('demo.cat', {
      resolveCustomTextEditor(document, page) {
        page.webview.html = `<pre>${document.getText()}</pre>`;
        log('editing', document.languageId, page.title);
      },
    }),
    vscode.window.registerCustomEditorProvider('demo.bytes', {
      openCustomDocument: (uri) => ({ uri, dispose: () => log('let go') }),
      resolveCustomEditor(document, page) {
        page.webview.html = document.uri.path.split('/').pop();
        setTimeout(() => page.dispose(), 0);
      },
    }),
  );
  log('ready');
};
"#;
        let Some((host, told, dir, log)) = hosted_logged("vscode-pages", code) else {
            return;
        };
        let host = Arc::new(host);
        let said = editor(&host, told, log, |method, _| match method {
            "webview.post" => Ok(json!(true)),
            other => Err(format!("no {other} here")),
        });
        write_file(&dir.join("tom.txt"), "meow");
        let file = format!("file://{}/tom.txt", dir.canonicalize().unwrap().display());
        host.notify("init", json!({ "folders": [dir] }));
        host.request("activate", json!({}), SOON).unwrap();
        written(&said, "ready");
        let pages = || -> Vec<Value> {
            lock(&said)
                .iter()
                .filter(|(method, _)| method == "webview")
                .map(|(_, params)| params.clone())
                .collect()
        };
        // A view that is a page is listed as a view, and is no page yet.
        let listed = lock(&said)
            .iter()
            .any(|(method, params)| method == "view" && params["kind"] == "webview");
        assert!(listed && pages().is_empty());

        assert!(
            host.request("webview.restore", json!({"viewType":"unknown"}), SOON)
                .is_err()
        );
        host.request(
            "webview.restore",
            json!({"viewType":"demo.saved","title":"Saved","state":{"count":7}}),
            SOON,
        )
        .unwrap();
        written(&said, r#"restored {"count":7}"#);
        assert_eq!(pages().last().unwrap()["html"], "<p>restored 7</p>");
        host.notify("webview.closed", json!({"id":"page1"}));
        // The next test's page names are relative to the first created.
        // A page: what it is called, where it goes, what it may do, and
        // then its text. What it may read is under a name of its own.
        host.request("executeCommand", json!({ "id": "demo.page" }), SOON)
            .unwrap();
        written(&said, "uri true page2 solder-resource://page2");
        let made = pages()
            .into_iter()
            .filter(|page| page["id"] == "page2")
            .collect::<Vec<_>>();
        let folder = dir.join("ext").display().to_string();
        assert_eq!(
            made[0],
            json!({
                "id": "page2", "viewType": "demo.page", "title": "Demo page", "column": 2, "html": "",
                "options": { "enableScripts": true, "localResourceRoots": [folder] },
            })
        );
        assert_eq!(made[1]["html"], "<p>hello</p>");
        // Messages both ways, what the editor says of it, and its end.
        let posted = host.request("executeCommand", json!({ "id": "demo.post" }), SOON);
        assert_eq!(posted, Ok(json!(true)));
        let post = lock(&said)
            .iter()
            .find(|(method, _)| method == "webview.post")
            .map(|(_, params)| params.clone());
        assert_eq!(post, Some(json!({ "id": "page2", "message": { "n": 1 } })));
        host.notify(
            "webview.message",
            json!({ "id": "page2", "message": { "hi": 1 } }),
        );
        written(&said, r#"got {"hi":1}"#);
        host.notify(
            "webview.state",
            json!({ "id": "page2", "visible": false, "active": false }),
        );
        written(&said, "state false false");
        host.notify("webview.closed", json!({ "id": "page2" }));
        written(&said, "disposed");
        assert_eq!(
            pages().last(),
            Some(&json!({ "id": "page2", "gone": true }))
        );
        // Gone, it takes no more messages.
        let posted = host.request("executeCommand", json!({ "id": "demo.post" }), SOON);
        assert_eq!(posted, Ok(json!(false)));

        // The view that is a page is filled in when it is asked for, with
        // scripts off unless it turns them on.
        host.request("webview.resolve", json!({ "id": "demo.side" }), SOON)
            .unwrap();
        written(&said, "resolved demo.side demo.side");
        let side = pages()
            .into_iter()
            .rfind(|page| page["id"] == "viewdemo.side")
            .unwrap();
        assert_eq!(
            (&side["html"], &side["options"]["enableScripts"]),
            (&json!("<b>side</b>"), &json!(false))
        );

        // An editor of its own for a file: one that edits text is given
        // the text, and one that reads the file itself is given where it
        // is and told when its page closed.
        let open = |kind: &str| {
            host.request(
                "customEditor.open",
                json!({ "viewType": kind, "uri": file }),
                SOON,
            )
        };
        open("demo.cat").unwrap();
        written(&said, "editing plaintext tom.txt");
        assert!(
            pages()
                .iter()
                .any(|page| page["html"] == "<pre>meow</pre>" && page["title"] == "tom.txt")
        );
        open("demo.bytes").unwrap();
        written(&said, "let go");
        assert!(pages().iter().any(|page| page["html"] == "tom.txt"));
        assert_eq!(open("demo.none"), Err("No editor demo.none".into()));
    }

    #[test]
    fn notebook_renderers_send_messages_only_to_their_open_pages() {
        let code = r#"
const v = require('vscode');
exports.activate = () => {
  const messages = v.notebooks.createRendererMessaging('counter');
  const received = [];
  messages.onDidReceiveMessage(event => received.push(event.message));
  v.commands.registerCommand('counter.send', message => messages.postMessage(message));
  v.commands.registerCommand('counter.received', () => received);
};
"#;
        let Some((host, told, _, log)) = hosted_logged("renderer-messages", code) else {
            return;
        };
        let host = Arc::new(host);
        let said = editor(&host, told, log, |method, _| match method {
            "webview.post" => Ok(json!(true)),
            _ => Err(format!("no {method}")),
        });
        host.request("activate", json!({}), SOON).unwrap();
        host.notify(
            "notebook.renderer.open",
            json!({"id":"one", "renderer":"counter", "uri":"file:///counter.book"}),
        );
        host.notify(
            "notebook.renderer.open",
            json!({"id":"other", "renderer":"plot", "uri":"file:///counter.book"}),
        );
        let run = |command: &str, args: Value| {
            host.request("executeCommand", json!({"id":command,"args":args}), SOON)
                .unwrap()
        };
        assert_eq!(run("counter.send", json!([{"count":1}])), json!(true));
        let all = lock(&said);
        let posts: Vec<_> = all
            .iter()
            .filter(|(method, _)| method == "webview.post")
            .map(|(_, params)| params.clone())
            .collect();
        assert_eq!(posts, [json!({"id":"one","message":{"count":1}})]);
        drop(all);
        host.notify(
            "notebook.renderer.message",
            json!({"renderer":"counter","uri":"file:///counter.book","message":{"count":2}}),
        );
        assert_eq!(run("counter.received", json!([])), json!([{"count":2}]));
        host.notify("notebook.renderer.closed", json!({"id":"one"}));
        assert_eq!(run("counter.send", json!([{"count":3}])), json!(false));
    }

    // First-party serializer with explicit gates, so a test can edit or
    // issue another save while the host is waiting without a timed race.
    const NOTEBOOK_SAVE_FIXTURE: &str = r#"
const v = require('vscode');
const fs = require('fs');
const crypto = require('crypto');
const childProcess = require('child_process');
const calls = [], releases = [], saves = [];
let mode = {}, reads = 0, runs = 0, denyReads = false;
const book = () => v.workspace.notebookDocuments[0];
const pause = () => new Promise((resolve) => releases.push(resolve));
const hash = (bytes) => crypto.createHash('sha256').update(bytes).digest('hex');
const packed = (data) => ({ metadata: data.metadata, cells: data.cells.map((cell) => ({
  kind: cell.kind, value: cell.value, languageId: cell.languageId, metadata: cell.metadata,
  executionSummary: cell.executionSummary,
  outputs: (cell.outputs || []).map((output) => ({ metadata: output.metadata,
    items: output.items.map((item) => ({ mime: item.mime, data: Buffer.from(item.data).toString('base64') })),
  })),
})) });
const openFile = fs.promises.open;
fs.promises.open = (...args) => {
  if (denyReads && args[1] === 'r') throw new Error('fixture source reads forbidden');
  return openFile(...args);
};
exports.activate = (context) => {
  context.subscriptions.push(
    v.workspace.registerNotebookSerializer('save-book', {
      async deserializeNotebook(bytes) {
        reads += 1;
        if (mode.openHold) await pause();
        if (mode.json) {
          const data = JSON.parse(Buffer.from(bytes).toString());
          for (const cell of data.cells) for (const output of cell.outputs || []) {
            for (const item of output.items) item.data = Buffer.from(item.data, 'base64');
          }
          return data;
        }
        return new v.NotebookData([new v.NotebookCellData(v.NotebookCellKind.Code, Buffer.from(bytes).toString(), 'plaintext')]);
      },
      async serializeNotebook(data) {
        calls.push(data.cells[0].value);
        if (mode.hold) await pause();
        if (mode.fail) throw new Error('fixture serializer failed');
        if (mode.json) return Buffer.from(JSON.stringify(packed(data)));
        return Buffer.from(data.cells[0].value);
      },
    }),
    v.workspace.onDidSaveNotebookDocument((notebook) => saves.push({
      bytes: fs.readFileSync(notebook.uri.fsPath, 'utf8'), dirty: notebook.isDirty,
    })),
    v.commands.registerCommand('demo.mode', (value) => { mode = value; }),
    v.commands.registerCommand('demo.release', () => { releases.shift()(); }),
    v.commands.registerCommand('demo.save', () => book().save()),
    v.notebooks.createNotebookController('fixture.runner', 'save-book', 'Fixture runner', (cells, notebook, controller) => {
      runs += 1;
      for (const cell of cells) {
        const run = controller.createNotebookCellExecution(cell);
        run.start(100);
        run.token.onCancellationRequested(() => run.end(false, 200));
      }
    }),
    v.commands.registerCommand('demo.denyReads', (value) => { denyReads = value; }),
    v.commands.registerCommand('demo.largeOutput', (size) => {
      book().cellAt(0).outputs = [new v.NotebookCellOutput([
        new v.NotebookCellOutputItem(Buffer.alloc(size, 255), 'application/octet-stream'),
      ], { large: true })];
    }),
    v.commands.registerCommand('demo.state', () => ({
      calls, saves, waiting: releases.length, dirty: book() && book().isDirty,
      reads, runs, version: book() && book().version, uri: book() && book().uri.toString(),
      running: book() && book()._running.size,
    })),
    v.commands.registerCommand('demo.time', (file) => fs.utimesSync(file, 1234567890, 1234567890)),
    v.commands.registerCommand('demo.external', (value) => {
      const file = book().uri.fsPath;
      const before = fs.statSync(file, { bigint: true });
      const original = fs.readFileSync(file);
      fs.writeFileSync(file, value);
      fs.utimesSync(file, Number(before.atimeNs) / 1e9, Number(before.mtimeNs) / 1e9);
      const after = fs.statSync(file, { bigint: true });
      return { sameSize: before.size === after.size, sameTime: before.mtimeNs === after.mtimeNs,
        sameHash: hash(original) === hash(fs.readFileSync(file)) };
    }),
    v.commands.registerCommand('demo.renameFailure', () => {
      const rename = fs.promises.rename;
      fs.promises.rename = async (...args) => {
        fs.promises.rename = rename;
        throw new Error('fixture rename failed');
      };
    }),
    v.commands.registerCommand('demo.stageHold', () => {
      const open = fs.promises.open;
      fs.promises.open = async (...args) => {
        const handle = await open(...args);
        if (args[1] === 'wx') {
          fs.promises.open = open;
          const sync = handle.sync.bind(handle);
          handle.sync = async () => { await sync(); await pause(); };
        }
        return handle;
      };
    }),
    v.commands.registerCommand('demo.collision', () => {
      const randomBytes = crypto.randomBytes;
      crypto.randomBytes = () => {
        crypto.randomBytes = randomBytes;
        return Buffer.alloc(16);
      };
    }),
    v.commands.registerCommand('demo.copyFailure', () => {
      const execFile = childProcess.execFile;
      childProcess.execFile = (...args) => {
        childProcess.execFile = execFile;
        process.nextTick(() => args[args.length - 1](new Error('fixture metadata copy failed')));
      };
    }),
    v.commands.registerCommand('demo.linkRace', (value) => {
      const link = fs.promises.link;
      fs.promises.link = async (from, to) => {
        fs.promises.link = link;
        fs.writeFileSync(to, value);
        return link(from, to);
      };
    }),
    v.commands.registerCommand('demo.clock', (elapsed) => {
      const now = Date.now();
      Date.now = () => now + elapsed;
    }),
    v.commands.registerCommand('demo.xattr', (name, value, file = book().uri.fsPath) => {
      childProcess.execFileSync('/usr/bin/xattr', ['-w', name, value, file]);
    }),
  );
};
"#;

    fn notebook_command(host: &VsHost, id: &str, args: Value) -> Value {
        host.request("executeCommand", json!({ "id": id, "args": args }), SOON)
            .unwrap()
    }

    fn notebook_open(host: &VsHost, file: &Path) -> String {
        let uri = format!("file://{}", file.display());
        host.request(
            "notebook.open",
            json!({ "type": "save-book", "uri": uri }),
            SOON,
        )
        .unwrap();
        uri
    }

    fn notebook_fixture(name: &str) -> Option<(VsHost, PathBuf, String)> {
        let (host, _told, dir) = hosted(name, NOTEBOOK_SAVE_FIXTURE)?;
        host.request("activate", json!({}), SOON).unwrap();
        let file = dir.join("save.book");
        std::fs::write(&file, "aaaa").unwrap();
        notebook_command(&host, "demo.time", json!([file.to_string_lossy()]));
        let uri = notebook_open(&host, &file);
        Some((host, file, uri))
    }

    fn notebook_edit(host: &VsHost, uri: &str, value: &str) {
        host.notify(
            "notebook.cell",
            json!({ "uri": uri, "handle": 0, "value": value }),
        );
    }

    fn notebook_waiting(host: &VsHost, calls: usize) {
        let until = std::time::Instant::now() + SOON;
        loop {
            let state = notebook_command(host, "demo.state", json!([]));
            if state["waiting"] == 1 && state["calls"].as_array().unwrap().len() == calls {
                return;
            }
            assert!(
                std::time::Instant::now() < until,
                "serializer did not wait: {state}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn notebook_no_staging(file: &Path) {
        assert!(
            std::fs::read_dir(file.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".solder-"))
        );
    }

    fn notebook_snapshot(host: &VsHost, uri: &str) -> Value {
        host.request("notebook.snapshot", json!({ "uri": uri }), SOON)
            .unwrap()
    }

    fn notebook_restore(host: &VsHost, uri: &str, snapshot: &Value) -> Answer {
        host.request(
            "notebook.restore",
            json!({ "uri": uri, "type": "save-book", "snapshot": snapshot }),
            SOON,
        )
    }

    fn notebook_prepare_copy(host: &VsHost, uri: &str, target: &Path) -> Value {
        host.request(
            "notebook.export.prepare",
            json!({ "uri": uri, "target": target.to_string_lossy() }),
            SOON,
        )
        .unwrap()
    }

    fn notebook_copy(host: &VsHost, uri: &str, target: &Path, token: &Value) -> Answer {
        host.request(
            "notebook.export",
            json!({ "uri": uri, "target": target.to_string_lossy(), "token": token }),
            SOON,
        )
    }

    fn notebook_restarted(host: &VsHost, file: &Path) -> VsHost {
        host.stop();
        let dir = file.parent().unwrap();
        let node = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join("node"))
            .find(|node| node.is_file())
            .unwrap();
        let host = VsHost::start(
            &node.to_string_lossy(),
            &dir.join("host/host.js"),
            &dir.join("ext"),
            &dir.join("storage"),
            &[],
            |_| {},
        )
        .unwrap();
        host.request("activate", json!({}), SOON).unwrap();
        host
    }

    fn notebook_rich_fixture(name: &str) -> Option<(VsHost, PathBuf, String, Value)> {
        let (host, _told, dir) = hosted(name, NOTEBOOK_SAVE_FIXTURE)?;
        host.request("activate", json!({}), SOON).unwrap();
        notebook_command(&host, "demo.mode", json!([{ "json": true }]));
        let data = json!({
            "metadata": { "custom": { "nested": [null, true, "kept"] } },
            "cells": [{
                "kind": 2, "value": "original", "languageId": "python",
                "metadata": { "collapsed": true, "attachments": { "name": "payload" } },
                "executionSummary": { "executionOrder": 7, "success": true,
                    "timing": { "startTime": 100, "endTime": 200 } },
                "outputs": [{ "metadata": { "display_id": "one" }, "items": [
                    { "mime": "text/plain", "data": "aGVsbG8K" },
                    { "mime": "application/octet-stream", "data": "AP8qgA==" },
                    { "mime": "image/png", "data": "AP8=" },
                ] }, { "metadata": { "empty": true }, "items": [] }],
            }, {
                "kind": 1, "value": "# Markdown", "languageId": "markdown",
                "metadata": { "tags": ["note"] }, "outputs": [],
            }],
        });
        let file = dir.join("rich.book");
        std::fs::write(&file, serde_json::to_vec(&data).unwrap()).unwrap();
        let uri = notebook_open(&host, &file);
        Some((host, file, uri, data))
    }

    #[test]
    fn notebook_recovery_keeps_complete_api_data_and_original_stamp_across_restart() {
        let Some((host, file, uri, mut data)) = notebook_rich_fixture("notebook-recovery-restart")
        else {
            return;
        };
        notebook_edit(&host, &uri, "recovered");
        data["cells"][0]["value"] = json!("recovered");
        let snapshot = notebook_snapshot(&host, &uri);
        assert_eq!(snapshot["data"], data);
        assert_eq!(snapshot["version"], 1);
        assert_eq!(snapshot["type"], "save-book");
        assert_eq!(snapshot["version_at_capture"], 2);
        assert_eq!(snapshot.as_object().unwrap().len(), 5);
        for field in ["size", "mtime", "dev", "ino"] {
            assert!(snapshot["revision"][field].is_string());
        }
        let opened = host
            .request(
                "notebook.open",
                json!({ "uri": uri, "type": "save-book" }),
                SOON,
            )
            .unwrap();
        let restarted = notebook_restarted(&host, &file);
        std::fs::write(&file, "external changes").unwrap();
        // Restore must work without any file read or deserializer call.
        notebook_command(&restarted, "demo.denyReads", json!([true]));
        assert_eq!(notebook_restore(&restarted, &uri, &snapshot), Ok(opened));
        assert_eq!(notebook_snapshot(&restarted, &uri), snapshot);
        let state = notebook_command(&restarted, "demo.state", json!([]));
        assert_eq!(state["reads"], 0);
        assert_eq!(state["runs"], 0);
        assert_eq!(state["running"], 0);
        assert_eq!(state["dirty"], true);
        assert_eq!(state["calls"], json!([]));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "external changes");
        notebook_command(&restarted, "demo.denyReads", json!([false]));
        notebook_command(&restarted, "demo.mode", json!([{ "json": true }]));
        assert!(
            restarted
                .request("notebook.save", json!({ "uri": uri }), SOON)
                .unwrap_err()
                .contains("changed on disk")
        );
        assert_eq!(notebook_snapshot(&restarted, &uri), snapshot);
        let target = file.with_file_name("rescued.book");
        let token = notebook_prepare_copy(&restarted, &uri, &target);
        assert_eq!(
            notebook_copy(&restarted, &uri, &target, &token),
            Ok(json!(true))
        );
        let copied: Value = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(copied, data);
        let state = notebook_command(&restarted, "demo.state", json!([]));
        assert_eq!(state["uri"], uri);
        assert_eq!(state["dirty"], true);
        assert_eq!(state["saves"], json!([]));
        assert_eq!(notebook_snapshot(&restarted, &uri), snapshot);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "external changes");

        let missing = notebook_restarted(&restarted, &file);
        std::fs::remove_file(&file).unwrap();
        assert!(notebook_restore(&missing, &uri, &snapshot).is_ok());
        assert!(!file.exists());
        assert_eq!(notebook_snapshot(&missing, &uri), snapshot);
        notebook_no_staging(&file);
    }

    #[test]
    fn notebook_snapshot_retains_outputs_larger_than_the_editor_preview_limit() {
        let Some((host, _file, uri)) = notebook_fixture("notebook-recovery-large-output") else {
            return;
        };
        let size: usize = (8 << 20) + 1;
        notebook_edit(&host, &uri, "mine");
        notebook_command(&host, "demo.largeOutput", json!([size]));
        let snapshot = notebook_snapshot(&host, &uri);
        let bytes = snapshot["data"]["cells"][0]["outputs"][0]["items"][0]["data"]
            .as_str()
            .unwrap();
        assert_eq!(bytes.len(), 4 * size.div_ceil(3));
        assert_eq!(
            snapshot["data"]["cells"][0]["outputs"][0]["metadata"],
            json!({ "large": true })
        );
        host.notify("notebook.close", json!({ "uri": uri }));
        let restored = notebook_restore(&host, &uri, &snapshot).unwrap();
        assert_eq!(restored["cells"][0]["outputs"][0]["items"][0]["size"], size);
        assert!(restored["cells"][0]["outputs"][0]["items"][0]["data"].is_null());
        assert_eq!(notebook_snapshot(&host, &uri), snapshot);
    }

    #[test]
    fn notebook_restore_refuses_legacy_invalid_and_dirty_replacements_and_resets_runs() {
        let Some((host, file, uri, _)) = notebook_rich_fixture("notebook-recovery-invalid") else {
            return;
        };
        let snapshot = notebook_snapshot(&host, &uri);
        let mut invalid = Vec::new();
        invalid.push(json!({ "cells": snapshot["data"]["cells"] }));
        invalid.push(Value::Null);
        for (pointer, value) in [
            ("/version", json!(2)),
            ("/type", json!("other-book")),
            ("/version_at_capture", json!(0)),
            ("/revision", Value::Null),
            ("/revision/size", json!(42)),
            ("/revision/ino", json!("-1")),
            ("/revision/sha256", json!("bad")),
            ("/revision/resolved", json!("relative.book")),
            ("/data/cells/0/kind", json!(3)),
            ("/data/cells/0/metadata", json!([])),
            ("/data/cells/0/executionSummary/success", json!("yes")),
            ("/data/cells/0/outputs/0/items/0/data", json!("not base64!")),
            ("/data/cells/0/outputs/0/items/0/data", json!("AB==")),
        ] {
            let mut broken = snapshot.clone();
            *broken.pointer_mut(pointer).unwrap() = value;
            invalid.push(broken);
        }
        let original = std::fs::read(&file).unwrap();
        for broken in invalid {
            assert!(
                notebook_restore(&host, &uri, &broken)
                    .unwrap_err()
                    .contains("Invalid or unsupported")
            );
            assert_eq!(notebook_snapshot(&host, &uri), snapshot);
        }
        notebook_edit(&host, &uri, "dirty known cells");
        let dirty = notebook_snapshot(&host, &uri);
        assert!(
            notebook_restore(&host, &uri, &snapshot)
                .unwrap_err()
                .contains("dirty notebook")
        );
        assert_eq!(notebook_snapshot(&host, &uri), dirty);
        assert_eq!(std::fs::read(&file).unwrap(), original);
        assert_eq!(notebook_command(&host, "demo.save", json!([])), true);
        host.request(
            "notebook.execute",
            json!({ "uri": uri, "handles": [0] }),
            SOON,
        )
        .unwrap();
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["running"],
            1
        );
        let restored = notebook_restore(&host, &uri, &snapshot).unwrap();
        assert_eq!(restored["runner"], "Fixture runner");
        assert_eq!(restored["cells"][0]["value"], "original");
        let state = notebook_command(&host, "demo.state", json!([]));
        assert_eq!(state["running"], 0);
        assert_eq!(state["runs"], 1);
        assert_eq!(state["dirty"], true);
        assert_eq!(notebook_snapshot(&host, &uri)["data"], snapshot["data"]);
    }

    #[test]
    fn notebook_snapshot_captures_cells_at_request_and_revision_after_preceding_save() {
        let Some((host, file, uri)) = notebook_fixture("notebook-recovery-order") else {
            return;
        };
        let original = notebook_snapshot(&host, &uri);
        notebook_edit(&host, &uri, "saved first");
        notebook_command(&host, "demo.mode", json!([{ "hold": true }]));
        let (tx, saved) = mpsc::channel();
        host.ask("notebook.save", json!({ "uri": uri }), move |answer| {
            tx.send(answer).unwrap();
        });
        notebook_waiting(&host, 1);
        notebook_edit(&host, &uri, "captured second");
        let (tx, captured) = mpsc::channel();
        host.ask("notebook.snapshot", json!({ "uri": uri }), move |answer| {
            tx.send(answer).unwrap();
        });
        notebook_command(&host, "demo.state", json!([]));
        assert!(captured.try_recv().is_err());
        notebook_edit(&host, &uri, "later third");
        notebook_command(&host, "demo.release", json!([]));
        assert_eq!(saved.recv_timeout(SOON).unwrap(), Ok(json!(true)));
        let snapshot = captured.recv_timeout(SOON).unwrap().unwrap();
        assert_eq!(snapshot["data"]["cells"][0]["value"], "captured second");
        assert_eq!(snapshot["version_at_capture"], 3);
        assert_ne!(snapshot["revision"], original["revision"]);
        assert_eq!(
            snapshot["revision"],
            notebook_snapshot(&host, &uri)["revision"]
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "saved first");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );
        notebook_command(&host, "demo.mode", json!([{}]));
        std::fs::write(&file, "changed outside").unwrap();
        assert!(
            host.request("notebook.save", json!({ "uri": uri }), SOON)
                .is_err()
        );
        assert_eq!(
            snapshot["revision"],
            notebook_snapshot(&host, &uri)["revision"]
        );
    }

    #[test]
    fn notebook_restore_supersedes_pending_opens_even_when_closed_before_they_finish() {
        let Some((host, file, uri)) = notebook_fixture("notebook-recovery-opening") else {
            return;
        };
        notebook_edit(&host, &uri, "recovered");
        let snapshot = notebook_snapshot(&host, &uri);
        host.notify("notebook.close", json!({ "uri": uri }));
        notebook_command(&host, "demo.mode", json!([{ "openHold": true }]));
        let open_async = || {
            let (tx, rx) = mpsc::channel();
            host.ask(
                "notebook.open",
                json!({ "uri": uri, "type": "save-book" }),
                move |answer| {
                    tx.send(answer).unwrap();
                },
            );
            rx
        };
        let early = open_async();
        notebook_waiting(&host, 0);
        let duplicate = open_async();
        let state = notebook_command(&host, "demo.state", json!([]));
        assert_eq!(state["reads"], 2);
        let restored = notebook_restore(&host, &uri, &snapshot).unwrap();
        notebook_command(&host, "demo.release", json!([]));
        assert_eq!(early.recv_timeout(SOON).unwrap(), Ok(restored.clone()));
        assert_eq!(duplicate.recv_timeout(SOON).unwrap(), Ok(restored));
        assert_eq!(notebook_snapshot(&host, &uri), snapshot);

        host.notify("notebook.close", json!({ "uri": uri }));
        let early = open_async();
        notebook_waiting(&host, 0);
        notebook_restore(&host, &uri, &snapshot).unwrap();
        host.notify("notebook.close", json!({ "uri": uri }));
        notebook_command(&host, "demo.release", json!([]));
        assert!(
            early
                .recv_timeout(SOON)
                .unwrap()
                .unwrap_err()
                .contains("superseded")
        );
        assert!(
            host.request("notebook.snapshot", json!({ "uri": uri }), SOON)
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "aaaa");
    }

    #[test]
    fn a_queued_notebook_restore_cannot_recreate_a_closed_view() {
        let Some((host, _file, uri)) = notebook_fixture("notebook-restore-closed") else {
            return;
        };
        let snapshot = notebook_snapshot(&host, &uri);
        notebook_command(&host, "demo.mode", json!([{ "hold": true }]));
        let (send, saving) = mpsc::channel();
        host.ask("notebook.save", json!({"uri":uri}), move |answer| {
            send.send(answer).unwrap();
        });
        notebook_waiting(&host, 1);
        let (send, restoring) = mpsc::channel();
        host.ask(
            "notebook.restore",
            json!({"uri":uri,"type":"save-book","snapshot":snapshot,"lifecycle":"old-view"}),
            move |answer| {
                send.send(answer).unwrap();
            },
        );
        notebook_command(&host, "demo.state", json!([]));
        assert!(restoring.try_recv().is_err());
        host.notify("notebook.close", json!({"uri":uri,"lifecycle":"old-view"}));
        notebook_command(&host, "demo.release", json!([]));
        assert!(saving.recv_timeout(SOON).unwrap().is_ok());
        assert!(
            restoring
                .recv_timeout(SOON)
                .unwrap()
                .unwrap_err()
                .contains("closed")
        );
        assert!(
            host.request("notebook.snapshot", json!({"uri":uri}), SOON)
                .is_err()
        );
        assert!(
            host.request(
                "notebook.restore",
                json!({"uri":uri,"type":"save-book","snapshot":snapshot,"lifecycle":"old-view"}),
                SOON
            )
            .is_err()
        );
        assert!(notebook_restore(&host, &uri, &snapshot).unwrap()["dirty"] == true);
        host.notify("notebook.close", json!({"uri":uri}));
        notebook_command(&host, "demo.mode", json!([{}]));
        host.request(
            "notebook.open",
            json!({"uri":uri,"type":"save-book","lifecycle":"new-view"}),
            SOON,
        )
        .unwrap();
        // An old completion must not close the view opened in its place.
        host.notify("notebook.close", json!({"uri":uri,"lifecycle":"old-view"}));
        assert!(
            host.request("notebook.snapshot", json!({"uri":uri}), SOON)
                .is_ok()
        );
        assert!(
            host.request(
                "notebook.restore",
                json!({"uri":uri,"type":"save-book","snapshot":snapshot,"lifecycle":"other-view"}),
                SOON
            )
            .is_err()
        );
    }

    #[test]
    fn notebook_copy_captures_request_data_queues_with_saves_and_keeps_source_state() {
        let Some((host, file, uri)) = notebook_fixture("notebook-copy-order") else {
            return;
        };
        let target = file.with_file_name("copy.book");
        let token = notebook_prepare_copy(&host, &uri, &target);
        notebook_edit(&host, &uri, "requested copy");
        let before = notebook_snapshot(&host, &uri);
        notebook_command(&host, "demo.mode", json!([{ "hold": true }]));
        let (tx, copied) = mpsc::channel();
        host.ask(
            "notebook.export",
            json!({ "uri": uri, "target": target.to_string_lossy(), "token": token }),
            move |answer| {
                tx.send(answer).unwrap();
            },
        );
        notebook_waiting(&host, 1);
        notebook_edit(&host, &uri, "later saved cells");
        let (tx, saved) = mpsc::channel();
        host.ask("notebook.save", json!({ "uri": uri }), move |answer| {
            tx.send(answer).unwrap();
        });
        notebook_command(&host, "demo.state", json!([]));
        notebook_command(&host, "demo.release", json!([]));
        assert_eq!(copied.recv_timeout(SOON).unwrap(), Ok(json!(true)));
        notebook_waiting(&host, 2);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "requested copy");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "aaaa");
        let state = notebook_command(&host, "demo.state", json!([]));
        assert_eq!(state["dirty"], true);
        assert_eq!(state["uri"], uri);
        assert_eq!(state["saves"], json!([]));
        assert!(
            notebook_copy(&host, &uri, &target, &token)
                .unwrap_err()
                .contains("token")
        );
        notebook_command(&host, "demo.release", json!([]));
        assert_eq!(saved.recv_timeout(SOON).unwrap(), Ok(json!(true)));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "later saved cells");
        assert_ne!(
            notebook_snapshot(&host, &uri)["revision"],
            before["revision"]
        );
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            false
        );
        notebook_no_staging(&file);
    }

    #[test]
    fn notebook_copy_conflicts_failures_and_exclusive_creation_preserve_both_files() {
        let Some((host, file, uri)) = notebook_fixture("notebook-copy-failed") else {
            return;
        };
        notebook_edit(&host, &uri, "mine");
        let before = notebook_snapshot(&host, &uri);
        let target = file.with_file_name("copy.book");
        let token = notebook_prepare_copy(&host, &uri, &target);
        notebook_command(&host, "demo.mode", json!([{ "fail": true }]));
        assert!(
            notebook_copy(&host, &uri, &target, &token)
                .unwrap_err()
                .contains("serializer failed")
        );
        assert!(!target.exists());
        assert!(
            notebook_copy(&host, &uri, &target, &token)
                .unwrap_err()
                .contains("token")
        );
        notebook_command(&host, "demo.mode", json!([{}]));

        let token = notebook_prepare_copy(&host, &uri, &target);
        notebook_command(&host, "demo.linkRace", json!(["arrived at publication"]));
        assert!(notebook_copy(&host, &uri, &target, &token).is_err());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "arrived at publication"
        );
        notebook_no_staging(&file);
        let token = notebook_prepare_copy(&host, &uri, &target);
        std::fs::write(&target, "changed after prepare").unwrap();
        assert!(
            notebook_copy(&host, &uri, &target, &token)
                .unwrap_err()
                .contains("changed on disk")
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "changed after prepare"
        );
        let token = notebook_prepare_copy(&host, &uri, &target);
        notebook_command(&host, "demo.stageHold", json!([]));
        let calls = notebook_command(&host, "demo.state", json!([]))["calls"]
            .as_array()
            .unwrap()
            .len();
        let (tx, copied) = mpsc::channel();
        host.ask(
            "notebook.export",
            json!({ "uri": uri, "target": target.to_string_lossy(), "token": token }),
            move |answer| {
                tx.send(answer).unwrap();
            },
        );
        notebook_waiting(&host, calls + 1);
        std::fs::write(&target, "changed during staging").unwrap();
        notebook_command(&host, "demo.release", json!([]));
        assert!(
            copied
                .recv_timeout(SOON)
                .unwrap()
                .unwrap_err()
                .contains("changed on disk")
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "changed during staging"
        );
        notebook_no_staging(&file);
        let token = notebook_prepare_copy(&host, &uri, &target);
        notebook_command(&host, "demo.renameFailure", json!([]));
        assert!(
            notebook_copy(&host, &uri, &target, &token)
                .unwrap_err()
                .contains("rename failed")
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "changed during staging"
        );
        let token = notebook_prepare_copy(&host, &uri, &target);
        std::fs::remove_file(&target).unwrap();
        assert!(notebook_copy(&host, &uri, &target, &token).is_err());
        assert!(!target.exists());
        assert_eq!(notebook_snapshot(&host, &uri), before);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "aaaa");
        let state = notebook_command(&host, "demo.state", json!([]));
        assert_eq!(state["dirty"], true);
        assert_eq!(state["uri"], uri);
        assert_eq!(state["saves"], json!([]));
        notebook_no_staging(&file);
    }

    #[test]
    fn notebook_copy_tokens_are_bound_one_use_bounded_and_expire() {
        let Some((host, file, uri)) = notebook_fixture("notebook-copy-tokens") else {
            return;
        };
        notebook_edit(&host, &uri, "mine");
        let target = file.with_file_name("copy.book");
        let first = notebook_prepare_copy(&host, &uri, &target);
        for _ in 0..128 {
            notebook_prepare_copy(&host, &uri, &target);
        }
        assert!(
            notebook_copy(&host, &uri, &target, &first)
                .unwrap_err()
                .contains("token")
        );
        let token = notebook_prepare_copy(&host, &uri, &target);
        let other = file.with_file_name("other.book");
        assert!(
            notebook_copy(&host, &uri, &other, &token)
                .unwrap_err()
                .contains("token")
        );
        assert!(
            notebook_copy(&host, &uri, &target, &token)
                .unwrap_err()
                .contains("token")
        );
        let token = notebook_prepare_copy(&host, &uri, &target);
        notebook_command(&host, "demo.clock", json!([6 * 60 * 1000]));
        assert!(
            notebook_copy(&host, &uri, &target, &token)
                .unwrap_err()
                .contains("token")
        );
        assert!(!target.exists());
        assert!(!other.exists());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "aaaa");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );
    }

    #[cfg(unix)]
    #[test]
    fn notebook_copy_survives_a_deleted_source_directory_and_refuses_dangling_aliases() {
        use std::os::unix::fs::symlink;
        let Some((host, _told, dir)) =
            hosted("notebook-copy-missing-source", NOTEBOOK_SAVE_FIXTURE)
        else {
            return;
        };
        host.request("activate", json!({}), SOON).unwrap();
        let project = dir.join("project");
        std::fs::create_dir(&project).unwrap();
        let file = project.join("source.book");
        std::fs::write(&file, "aaaa").unwrap();
        let uri = notebook_open(&host, &file);
        notebook_edit(&host, &uri, "mine");
        let before = notebook_snapshot(&host, &uri);
        std::fs::remove_dir_all(&project).unwrap();
        let target = dir.join("rescued.book");
        let token = notebook_prepare_copy(&host, &uri, &target);
        assert_eq!(notebook_copy(&host, &uri, &target, &token), Ok(json!(true)));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine");
        assert_eq!(notebook_snapshot(&host, &uri), before);
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );

        std::fs::create_dir(&project).unwrap();
        let late = dir.join("late.book");
        let token = notebook_prepare_copy(&host, &uri, &late);
        symlink(&late, &file).unwrap();
        assert!(
            notebook_copy(&host, &uri, &late, &token)
                .unwrap_err()
                .contains("source")
        );
        assert!(!late.exists());
        assert_eq!(notebook_snapshot(&host, &uri), before);
        assert!(
            host.request(
                "notebook.export.prepare",
                json!({ "uri": uri, "target": late.to_string_lossy() }),
                SOON
            )
            .unwrap_err()
            .contains("source")
        );
        notebook_no_staging(&target);
    }

    #[cfg(unix)]
    #[test]
    fn notebook_copy_refuses_source_aliases_and_preserves_destination_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let Some((host, file, uri)) = notebook_fixture("notebook-copy-alias") else {
            return;
        };
        let alias = file.with_file_name("alias.book");
        let hardlink = file.with_file_name("hardlink.book");
        symlink(&file, &alias).unwrap();
        std::fs::hard_link(&file, &hardlink).unwrap();
        for target in [&file, &alias, &hardlink] {
            assert!(
                host.request(
                    "notebook.export.prepare",
                    json!({ "uri": uri, "target": target.to_string_lossy() }),
                    SOON
                )
                .unwrap_err()
                .contains("source")
            );
        }
        std::fs::remove_file(&hardlink).unwrap();
        let target = file.with_file_name("copy.book");
        std::fs::write(&target, "destination").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        #[cfg(target_os = "macos")]
        if Path::new("/usr/bin/xattr").is_file() {
            notebook_command(
                &host,
                "demo.xattr",
                json!(["com.solder.copy-test", "source"]),
            );
            notebook_command(
                &host,
                "demo.xattr",
                json!([
                    "com.solder.copy-test",
                    "destination",
                    target.to_string_lossy()
                ]),
            );
        }
        notebook_edit(&host, &uri, "mine");
        let before = notebook_snapshot(&host, &uri);
        let token = notebook_prepare_copy(&host, &uri, &target);
        assert_eq!(notebook_copy(&host, &uri, &target, &token), Ok(json!(true)));
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine");
        #[cfg(target_os = "macos")]
        if Path::new("/usr/bin/xattr").is_file() {
            let output = Command::new("/usr/bin/xattr")
                .args(["-p", "com.solder.copy-test"])
                .arg(&target)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap().trim(),
                "destination"
            );
        }
        assert_eq!(notebook_snapshot(&host, &uri), before);
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );

        let token = notebook_prepare_copy(&host, &uri, &target);
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o440)).unwrap();
        assert!(
            notebook_copy(&host, &uri, &target, &token)
                .unwrap_err()
                .contains("read-only")
        );
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let token = notebook_prepare_copy(&host, &uri, &target);
        symlink(&target, file.with_file_name("target-symlink.book")).unwrap();
        std::fs::hard_link(&target, &hardlink).unwrap();
        assert!(
            notebook_copy(&host, &uri, &target, &token)
                .unwrap_err()
                .contains("hard links")
        );
        assert_eq!(notebook_snapshot(&host, &uri), before);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "aaaa");
        notebook_no_staging(&file);
    }

    #[test]
    fn notebook_failed_serializer_and_rename_keep_bytes_revision_and_dirty() {
        let Some((host, file, uri)) = notebook_fixture("notebook-save-failed") else {
            return;
        };
        notebook_edit(&host, &uri, "mine");
        notebook_command(&host, "demo.mode", json!([{ "fail": true }]));
        let save = || host.request("notebook.save", json!({ "uri": uri }), SOON);
        assert!(save().unwrap_err().contains("fixture serializer failed"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "aaaa");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );
        notebook_no_staging(&file);

        notebook_command(&host, "demo.mode", json!([{}]));
        notebook_command(&host, "demo.renameFailure", json!([]));
        assert!(save().unwrap_err().contains("fixture rename failed"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "aaaa");
        let state = notebook_command(&host, "demo.state", json!([]));
        assert_eq!(state["dirty"], true);
        assert_eq!(state["saves"], json!([]));
        notebook_no_staging(&file);
        assert_eq!(save(), Ok(json!(true)));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn notebook_failed_metadata_copy_keeps_source_and_cleans_staging() {
        let Some((host, file, uri)) = notebook_fixture("notebook-save-copy-failed") else {
            return;
        };
        notebook_edit(&host, &uri, "mine");
        notebook_command(&host, "demo.copyFailure", json!([]));
        let error = host
            .request("notebook.save", json!({ "uri": uri }), SOON)
            .unwrap_err();
        assert!(error.contains("fixture metadata copy failed"), "{error}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "aaaa");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );
        notebook_no_staging(&file);
        assert_eq!(notebook_command(&host, "demo.save", json!([])), true);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn notebook_atomic_saves_preserve_extended_attributes() {
        if !Path::new("/usr/bin/xattr").is_file() {
            return;
        }
        let Some((host, file, uri)) = notebook_fixture("notebook-save-xattr") else {
            return;
        };
        let name = "com.solder.save-test";
        notebook_command(&host, "demo.xattr", json!([name, "kept"]));
        for value in ["mine", "again"] {
            notebook_edit(&host, &uri, value);
            assert_eq!(notebook_command(&host, "demo.save", json!([])), true);
            let output = Command::new("/usr/bin/xattr")
                .args(["-p", name])
                .arg(&file)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "kept");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), value);
        }
        notebook_no_staging(&file);
    }

    #[test]
    fn notebook_save_refuses_changed_hash_even_with_same_size_and_mtime() {
        let Some((host, file, uri)) = notebook_fixture("notebook-save-hash") else {
            return;
        };
        notebook_edit(&host, &uri, "mine");
        assert_eq!(
            notebook_command(&host, "demo.external", json!(["bbbb"])),
            json!({ "sameSize": true, "sameTime": true, "sameHash": false })
        );
        for _ in 0..2 {
            let error = host
                .request("notebook.save", json!({ "uri": uri }), SOON)
                .unwrap_err();
            assert!(error.contains("changed on disk"), "{error}");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "bbbb");
            assert_eq!(
                notebook_command(&host, "demo.state", json!([]))["dirty"],
                true
            );
            notebook_no_staging(&file);
        }
        // A refused save must not adopt the conflicting file's revision.
        notebook_command(&host, "demo.external", json!(["aaaa"]));
        assert_eq!(
            host.request("notebook.save", json!({ "uri": uri }), SOON),
            Ok(json!(true))
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
    }

    #[test]
    fn notebook_save_rechecks_source_after_staging_and_removes_only_its_temp() {
        let Some((host, file, uri)) = notebook_fixture("notebook-save-final-check") else {
            return;
        };
        notebook_edit(&host, &uri, "mine");
        notebook_command(&host, "demo.stageHold", json!([]));
        let (tx, saved) = mpsc::channel();
        host.ask("notebook.save", json!({ "uri": uri }), move |answer| {
            tx.send(answer).unwrap();
        });
        notebook_waiting(&host, 1);
        assert_eq!(
            notebook_command(&host, "demo.external", json!(["bbbb"])),
            json!({ "sameSize": true, "sameTime": true, "sameHash": false })
        );
        notebook_command(&host, "demo.release", json!([]));
        assert!(
            saved
                .recv_timeout(SOON)
                .unwrap()
                .unwrap_err()
                .contains("changed on disk")
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "bbbb");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );
        notebook_no_staging(&file);
        notebook_command(&host, "demo.external", json!(["aaaa"]));
        assert_eq!(notebook_command(&host, "demo.save", json!([])), true);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
    }

    #[test]
    fn notebook_editor_and_extension_saves_queue_snapshots_and_keep_later_edits_dirty() {
        let Some((host, file, uri)) = notebook_fixture("notebook-save-queue") else {
            return;
        };
        notebook_edit(&host, &uri, "first");
        notebook_command(&host, "demo.mode", json!([{ "hold": true }]));
        let (tx, first) = mpsc::channel();
        host.ask("notebook.save", json!({ "uri": uri }), move |answer| {
            tx.send(answer).unwrap();
        });
        notebook_waiting(&host, 1);
        notebook_edit(&host, &uri, "second");
        let (tx, second) = mpsc::channel();
        host.ask(
            "executeCommand",
            json!({ "id": "demo.save" }),
            move |answer| {
                tx.send(answer).unwrap();
            },
        );
        // This command is a barrier: the second save has captured its cells.
        let state = notebook_command(&host, "demo.state", json!([]));
        assert_eq!(state["calls"], json!(["first"]));
        notebook_edit(&host, &uri, "third");
        notebook_command(&host, "demo.release", json!([]));
        assert_eq!(first.recv_timeout(SOON).unwrap(), Ok(json!(true)));
        notebook_waiting(&host, 2);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );
        notebook_command(&host, "demo.release", json!([]));
        assert_eq!(second.recv_timeout(SOON).unwrap(), Ok(json!(true)));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");
        let state = notebook_command(&host, "demo.state", json!([]));
        assert_eq!(state["dirty"], true);
        assert_eq!(state["calls"], json!(["first", "second"]));
        assert_eq!(
            state["saves"],
            json!([
                { "bytes": "first", "dirty": true }, { "bytes": "second", "dirty": true },
            ])
        );
        notebook_command(&host, "demo.mode", json!([{}]));
        assert_eq!(notebook_command(&host, "demo.save", json!([])), true);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "third");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            false
        );
        notebook_no_staging(&file);
    }

    #[test]
    fn notebook_successive_saves_advance_the_expected_revision() {
        let Some((host, file, uri)) = notebook_fixture("notebook-save-repeat") else {
            return;
        };
        for value in ["one", "two", "three"] {
            notebook_edit(&host, &uri, value);
            assert_eq!(notebook_command(&host, "demo.save", json!([])), true);
            assert_eq!(std::fs::read_to_string(&file).unwrap(), value);
            assert_eq!(
                notebook_command(&host, "demo.state", json!([]))["dirty"],
                false
            );
            notebook_no_staging(&file);
        }
    }

    #[test]
    fn notebook_source_revision_is_captured_before_async_deserialization() {
        let Some((host, _told, dir)) = hosted("notebook-open-revision", NOTEBOOK_SAVE_FIXTURE)
        else {
            return;
        };
        host.request("activate", json!({}), SOON).unwrap();
        notebook_command(&host, "demo.mode", json!([{ "openHold": true }]));
        let file = dir.join("save.book");
        std::fs::write(&file, "aaaa").unwrap();
        let uri = format!("file://{}", file.display());
        let (tx, opened) = mpsc::channel();
        host.ask(
            "notebook.open",
            json!({ "type": "save-book", "uri": uri }),
            move |answer| {
                tx.send(answer).unwrap();
            },
        );
        notebook_waiting(&host, 0);
        std::fs::write(&file, "bbbb").unwrap();
        notebook_command(&host, "demo.release", json!([]));
        assert_eq!(
            opened.recv_timeout(SOON).unwrap().unwrap()["cells"][0]["value"],
            "aaaa"
        );
        notebook_edit(&host, &uri, "mine");
        assert!(
            host.request("notebook.save", json!({ "uri": uri }), SOON)
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "bbbb");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );
    }

    #[test]
    fn notebook_temp_collision_does_not_delete_an_unowned_file() {
        let Some((host, file, uri)) = notebook_fixture("notebook-save-collision") else {
            return;
        };
        let other = file
            .parent()
            .unwrap()
            .join(".save.book.solder-00000000000000000000000000000000.tmp");
        std::fs::write(&other, "not ours").unwrap();
        notebook_edit(&host, &uri, "mine");
        notebook_command(&host, "demo.collision", json!([]));
        assert!(
            host.request("notebook.save", json!({ "uri": uri }), SOON)
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "aaaa");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "not ours");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );
    }

    #[cfg(unix)]
    #[test]
    fn notebook_saves_preserve_basic_mode_and_refuse_readonly_and_hardlinks() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let Some((host, file, uri)) = notebook_fixture("notebook-save-mode") else {
            return;
        };
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let owner = std::fs::metadata(&file).unwrap();
        notebook_edit(&host, &uri, "mine");
        assert_eq!(notebook_command(&host, "demo.save", json!([])), true);
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o640);
        assert_eq!(std::fs::metadata(&file).unwrap().uid(), owner.uid());
        assert_eq!(std::fs::metadata(&file).unwrap().gid(), owner.gid());
        let inode = std::fs::metadata(&file).unwrap().ino();
        notebook_edit(&host, &uri, "next");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o440)).unwrap();
        let error = host
            .request("notebook.save", json!({ "uri": uri }), SOON)
            .unwrap_err();
        assert!(error.contains("read-only"), "{error}");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let alias = file.with_extension("alias");
        std::fs::hard_link(&file, &alias).unwrap();
        let error = host
            .request("notebook.save", json!({ "uri": uri }), SOON)
            .unwrap_err();
        assert!(error.contains("hard links"), "{error}");
        assert_eq!(std::fs::metadata(&file).unwrap().ino(), inode);
        assert_eq!(std::fs::metadata(&alias).unwrap().ino(), inode);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(&alias).unwrap(), "mine");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );
        notebook_no_staging(&file);
    }

    #[cfg(unix)]
    #[test]
    fn notebook_saves_follow_original_symlink_target_and_refuse_retargeting() {
        use std::os::unix::fs::symlink;
        let Some((host, _told, dir)) = hosted("notebook-save-symlink", NOTEBOOK_SAVE_FIXTURE)
        else {
            return;
        };
        host.request("activate", json!({}), SOON).unwrap();
        let file = dir.join("save.book");
        let original = dir.join("original.book");
        let other = dir.join("other.book");
        std::fs::write(&original, "aaaa").unwrap();
        std::fs::write(&other, "mine").unwrap();
        symlink(&original, &file).unwrap();
        let uri = notebook_open(&host, &file);
        notebook_edit(&host, &uri, "mine");
        assert_eq!(notebook_command(&host, "demo.save", json!([])), true);
        assert!(
            std::fs::symlink_metadata(&file)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "mine");
        std::fs::remove_file(&file).unwrap();
        symlink(&other, &file).unwrap();
        notebook_edit(&host, &uri, "next");
        let error = host
            .request("notebook.save", json!({ "uri": uri }), SOON)
            .unwrap_err();
        assert!(error.contains("changed on disk"), "{error}");
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "mine");
        assert_eq!(
            notebook_command(&host, "demo.state", json!([]))["dirty"],
            true
        );
        notebook_no_staging(&file);
    }

    /// A notebook is read and written by its extension, which also runs
    /// its cells and says what came out.
    #[test]
    fn an_extension_reads_runs_and_writes_a_notebook() {
        let code = r#"
const vscode = require('vscode');
// A file of cells set apart by a line of dashes, each begun by what it is.
const reader = {
  deserializeNotebook(bytes) {
    const cells = Buffer.from(bytes).toString().split('\n---\n').filter(Boolean).map((part) => {
      const [head, ...rest] = part.split('\n');
      const code = head.startsWith('code:');
      return new vscode.NotebookCellData(code ? vscode.NotebookCellKind.Code : vscode.NotebookCellKind.Markup, rest.join('\n'), code ? head.slice(5) : 'markdown');
    });
    return new vscode.NotebookData(cells);
  },
  serializeNotebook(data) {
    const parts = data.cells.map((cell) => `${cell.kind === vscode.NotebookCellKind.Code ? 'code:' + cell.languageId : 'text'}\n${cell.value}`);
    return Buffer.from(parts.join('\n---\n'));
  },
};
exports.activate = (context) => {
  let runs = 0;
  const changes = [];
  const controller = vscode.notebooks.createNotebookController('demo.runner', 'demo-book', 'Demo runner', async (cells, notebook, controller) => {
    for (const cell of cells) {
      const run = controller.createNotebookCellExecution(cell);
      run.executionOrder = ++runs;
      run.start();
      const text = cell.document.getText();
      if (text === 'wait') {
        await new Promise((resolve) => run.token.onCancellationRequested(resolve));
        run.end(false);
        continue;
      }
      await run.replaceOutput(new vscode.NotebookCellOutput([vscode.NotebookCellOutputItem.stdout('running\n')]));
      if (text.includes('boom')) {
        await run.appendOutput(new vscode.NotebookCellOutput([vscode.NotebookCellOutputItem.error(new RangeError('boom'))]));
        run.end(false);
        continue;
      }
      await run.appendOutput(new vscode.NotebookCellOutput([
        vscode.NotebookCellOutputItem.text(text.toUpperCase()),
        new vscode.NotebookCellOutputItem(Uint8Array.from([137, 80, 78, 71]), 'image/png'),
        new vscode.NotebookCellOutputItem(Uint8Array.from([1, 2, 3]), 'application/x-thing'),
      ]));
      run.end(true);
    }
  });
  context.subscriptions.push(
    controller,
    vscode.workspace.registerNotebookSerializer('demo-book', reader),
    vscode.workspace.onDidChangeNotebookDocument((event) => changes.push(event)),
    vscode.commands.registerCommand('demo.books', () => ({
      open: vscode.workspace.notebookDocuments.map((notebook) => `${notebook.notebookType} ${notebook.cellCount} ${notebook.isDirty}`),
      cells: vscode.workspace.textDocuments.filter((document) => document.uri.scheme === 'vscode-notebook-cell').map((document) => `${document.languageId}:${document.getText()}`),
      front: vscode.window.activeNotebookEditor && vscode.window.activeNotebookEditor.selection.start,
      changes: changes.length,
      moved: changes.filter((event) => event.contentChanges.length).map((event) => event.contentChanges.map((change) => [change.range.start, change.range.end, change.removedCells.length, change.addedCells.length])),
    })),
  );
};
"#;
        let Some((host, told, dir, log)) = hosted_logged("vscode-notebook", code) else {
            return;
        };
        let host = Arc::new(host);
        let said = editor(&host, told, log, |method, _| {
            Err(format!("no {method} here"))
        });
        let file = dir.join("first.book");
        std::fs::write(&file, "text\n# Title\n---\ncode:javascript\nhello").unwrap();
        let uri = format!("file://{}", file.display());
        // Nothing reads a kind of notebook before its extension says it does.
        let early = host.request(
            "notebook.open",
            json!({ "type": "demo-book", "uri": uri }),
            SOON,
        );
        assert!(early.unwrap_err().contains("reads no notebooks"));
        host.request("activate", json!({}), SOON).unwrap();
        let heard = |method: &str| -> Vec<Value> {
            let all = lock(&said);
            let of = all.iter().filter(|(said, _)| said == method);
            of.map(|(_, params)| params.clone()).collect()
        };
        let until = |what: &str, done: &dyn Fn() -> bool| {
            let until = std::time::Instant::now() + SOON;
            while !done() {
                assert!(std::time::Instant::now() < until, "no {what}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        until("runner", &|| !heard("notebook.runners").is_empty());
        assert_eq!(
            heard("notebook.runners")[0],
            json!({ "runners": [{ "type": "demo-book", "label": "Demo runner" }] })
        );

        // Opened: the cells as the extension read them, and what runs them.
        let opened = host
            .request(
                "notebook.open",
                json!({ "type": "demo-book", "uri": uri }),
                SOON,
            )
            .unwrap();
        assert_eq!(
            opened,
            json!({ "runner": "Demo runner", "dirty": false, "version": 1, "cells": [
                { "handle": 0, "code": false, "language": "markdown", "value": "# Title", "outputs": [] },
                { "handle": 1, "code": true, "language": "javascript", "value": "hello", "outputs": [] },
            ] })
        );
        // The editor says what is typed into a cell, and which cells there
        // are after one was added before the others.
        host.notify(
            "notebook.cell",
            json!({ "uri": uri, "handle": 1, "value": "hello there" }),
        );
        host.notify(
            "notebook.cells",
            json!({ "uri": uri, "cells": [
                { "handle": 2, "code": true, "language": "javascript", "value": "boom" },
                { "handle": 0 }, { "handle": 1 },
            ] }),
        );
        host.notify("notebook.front", json!({ "uri": uri, "selected": 2 }));
        let books = || {
            host.request("executeCommand", json!({ "id": "demo.books" }), SOON)
                .unwrap()
        };
        assert_eq!(
            books(),
            json!({
                "open": ["demo-book 3 true"],
                "cells": ["markdown:# Title", "javascript:hello there", "javascript:boom"],
                "front": 2,
                "changes": 2,
                "moved": [[[0, 0, 0, 1]]],
            })
        );

        // Run: each cell says it runs, what it puts out as it comes, and
        // how it ended. A picture goes as it is, and of a form the editor
        // draws nothing of, what it is and how large.
        let ran = host.request(
            "notebook.execute",
            json!({ "uri": uri, "handles": [1, 2, 0] }),
            SOON,
        );
        assert_eq!(ran, Ok(json!(true)));
        until("the runs to end", &|| heard("notebook.run").len() == 4);
        assert_eq!(
            heard("notebook.run"),
            [
                json!({ "uri": uri, "handle": 2, "running": true }),
                json!({ "uri": uri, "handle": 2, "running": false, "failed": true, "order": 1, "version": 6 }),
                json!({ "uri": uri, "handle": 1, "running": true }),
                json!({ "uri": uri, "handle": 1, "running": false, "failed": false, "order": 2, "version": 9 }),
            ]
        );
        let outputs = heard("notebook.outputs");
        let last = |handle: u64| {
            let of = outputs.iter().rev().find(|said| said["handle"] == handle);
            of.unwrap()["outputs"].clone()
        };
        let stdout = json!({ "items": [{ "mime": "application/vnd.code.notebook.stdout", "size": 8, "text": "running\n" }] });
        let failed = last(2);
        assert_eq!(failed[0], stdout);
        let error = &failed[1]["items"][0];
        assert_eq!(error["mime"], "application/vnd.code.notebook.error");
        assert!(
            error["text"]
                .as_str()
                .unwrap()
                .starts_with("RangeError: boom"),
            "{error}"
        );
        assert_eq!(
            last(1),
            json!([stdout, { "items": [
                { "mime": "text/plain", "size": 11, "text": "HELLO THERE" },
                { "mime": "image/png", "size": 4, "picture": "iVBORw==" },
                { "mime": "application/x-thing", "size": 3, "data": "AQID" },
            ] }])
        );

        // Saved by the extension, from the cells as they are now.
        assert_eq!(
            host.request("notebook.save", json!({ "uri": uri }), SOON),
            Ok(json!(true))
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "code:javascript\nboom\n---\ntext\n# Title\n---\ncode:javascript\nhello there"
        );
        assert_eq!(books()["open"], json!(["demo-book 3 false"]));

        // A run that does not end by itself is told to stop.
        host.notify(
            "notebook.cell",
            json!({ "uri": uri, "handle": 1, "value": "wait" }),
        );
        let waiting = {
            let (host, uri) = (host.clone(), uri.clone());
            std::thread::spawn(move || {
                host.request(
                    "notebook.execute",
                    json!({ "uri": uri, "handles": [1] }),
                    SOON,
                )
            })
        };
        until("the run to start", &|| heard("notebook.run").len() == 5);
        assert_eq!(
            host.request("notebook.interrupt", json!({ "uri": uri }), SOON),
            Ok(json!(true))
        );
        assert_eq!(waiting.join().unwrap(), Ok(json!(true)));
        assert_eq!(heard("notebook.run")[5]["failed"], true);

        // Closed, it and the text of its cells are gone for the extension.
        host.notify("notebook.close", json!({ "uri": uri }));
        let after = books();
        assert_eq!(
            (&after["open"], &after["cells"], &after["front"]),
            (&json!([]), &json!([]), &Value::Null)
        );
    }

    #[test]
    fn an_extension_wakes_for_the_events_it_waits_for() {
        let events = |list: &[&str]| list.iter().map(|e| e.to_string()).collect::<Vec<_>>();
        let waits = events(&["onLanguage:rust", "onCommand:demo.run"]);
        assert!(wakes(&waits, "onLanguage:rust"));
        assert!(wakes(&waits, "onCommand:demo.run"));
        assert!(!wakes(&waits, "onLanguage:go") && !wakes(&waits, "*"));
        // One that waits for nothing, or for the start to be over, is
        // started once the editor is up.
        assert!(wakes(&events(&["*"]), "onLanguage:go"));
        assert!(wakes(&events(&["onStartupFinished"]), "*"));
        assert!(!wakes(&events(&["onStartupFinished"]), "onLanguage:go"));
        assert!(!wakes(&[], "*"));
    }
    #[test]
    fn trees_keep_actions_bound_to_nodes_when_a_branch_is_refreshed() {
        let code = r#"
const v = require('vscode');
exports.activate = (context) => {
  const changes = new v.EventEmitter();
  const a = {label:'A'}, b = {label:'B'}, child = {label:'Child'};
  const tree = v.window.createTreeView('demo', { treeDataProvider: {
    onDidChangeTreeData: changes.event,
    getChildren: (parent) => parent ? [child] : [a, b],
    getTreeItem: (node) => Object.assign(new v.TreeItem(node.label, node === a ? 1 : 0), {
      id: node.label, command: {command:'choose',arguments:[node.label]},
    }),
  }});
  tree.onDidChangeSelection(({selection}) => console.log('selected', selection[0].label));
  tree.onDidExpandElement(({element}) => console.log('expanded', element.label));
  tree.onDidChangeVisibility(({visible}) => console.log('visible', visible));
  context.subscriptions.push(tree);
  v.commands.registerCommand('choose', (label) => console.log('chosen', label));
  v.commands.registerCommand('refresh', () => { changes.fire(); tree.title = 'New title'; });
};
"#;
        let Some((host, told, dir)) = hosted("vscode-trees", code) else {
            return;
        };
        host.request("activate", json!({}), SOON).unwrap();
        let roots = host
            .request("view.children", json!({"id":"demo"}), SOON)
            .unwrap();
        let a = &roots[0]["key"];
        let b_action = &roots[1]["run"];
        assert_eq!(roots[0]["label"], "A");
        let child = host
            .request("view.children", json!({"id":"demo", "node":a}), SOON)
            .unwrap();
        host.request("view.children", json!({"id":"demo", "node":a}), SOON)
            .unwrap();
        // Refreshing A neither renumbers B's callback nor forgets its node.
        host.request("view.run", json!({"id":"demo", "run":b_action}), SOON)
            .unwrap();
        heard(
            &told,
            |told| matches!(told, Told::Log { text, .. } if text == "chosen B"),
        );
        host.request(
            "view.run",
            json!({"id":"demo", "run":child[0]["run"]}),
            SOON,
        )
        .unwrap();
        heard(
            &told,
            |told| matches!(told, Told::Log { text, .. } if text == "chosen Child"),
        );
        host.request(
            "view.expand",
            json!({"id":"demo", "node":a, "open":true}),
            SOON,
        )
        .unwrap();
        heard(
            &told,
            |told| matches!(told, Told::Log { text, .. } if text == "expanded A"),
        );
        host.request("view.visible", json!({"id":"demo", "visible":true}), SOON)
            .unwrap();
        heard(
            &told,
            |told| matches!(told, Told::Log { text, .. } if text == "visible true"),
        );
        host.request("executeCommand", json!({"id":"refresh"}), SOON)
            .unwrap();
        heard(
            &told,
            |told| matches!(told, Told::Said { method, params } if method == "view" && params["title"] == "New title"),
        );
        host.request("deactivate", json!({}), SOON).unwrap();
        assert_eq!(
            host.request("view.children", json!({"id":"demo"}), SOON),
            Ok(json!([]))
        );
        host.stop();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn source_control_tests_and_file_decorators_send_their_native_model() {
        let code = r#"
const v = require('vscode');
exports.activate = (context) => {
  const oldControl = v.scm.createSourceControl('demo', 'Old changes');
  const control = v.scm.createSourceControl('demo', 'Changes', v.Uri.file('/project'));
  oldControl.dispose();
  control.inputBox.value = 'Recorded';
  const group = control.createResourceGroup('modified', 'Modified');
  group.resourceStates = [{resourceUri:v.Uri.file('/project/one.rs'), command:{command:'commit'}}];
  control.acceptInputCommand = {command:'commit',title:'Commit'};
  v.commands.registerCommand('commit', () => console.log('commit',control.inputBox.value));
  const oldTests = v.tests.createTestController('demo','Old tests');
  const tests = v.tests.createTestController('demo','Tests');
  oldTests.dispose();
  tests.resolveHandler = (parent) => {
    if (parent) parent.children.add(tests.createTestItem('child','Child'));
    else { const test = tests.createTestItem('suite','Suite'); test.canResolveChildren = true; tests.items.add(test); }
  };
  tests.createRunProfile('Run',v.TestRunProfileKind.Run,(request) => {
    const run = tests.createTestRun(request,'Once');
    const item = request.include ? request.include[0] : tests.items.get('suite');
    run.started(item); run.failed(item, new v.TestMessage('Expected one\nFound two'),2);
    run.appendOutput('ran\r\n'); run.end();
  },true);
  context.subscriptions.push(control, tests, v.window.registerFileDecorationProvider({
    provideFileDecoration:(uri) => ({badge:'M',tooltip:uri.fsPath}),
  }));
};
"#;
        let Some((host, told, dir)) = hosted("vscode-view-models", code) else {
            return;
        };
        host.request("activate", json!({}), SOON).unwrap();
        let roots = host
            .request("view.children", json!({"id":"scm:demo"}), SOON)
            .unwrap();
        assert_eq!(roots[0]["label"], "Recorded");
        let resources = host
            .request(
                "view.children",
                json!({"id":"scm:demo","node":roots[2]["key"]}),
                SOON,
            )
            .unwrap();
        assert_eq!(resources[0]["label"], "one.rs");
        host.request(
            "view.run",
            json!({"id":"scm:demo","run":resources[0]["run"]}),
            SOON,
        )
        .unwrap();
        heard(
            &told,
            |told| matches!(told, Told::Log { text, .. } if text == "commit Recorded"),
        );
        let roots = host
            .request("view.children", json!({"id":"tests:demo"}), SOON)
            .unwrap();
        assert_eq!(roots[0]["label"], "Run all");
        assert_eq!(roots[1]["label"], "Suite");
        let children = host
            .request(
                "view.children",
                json!({"id":"tests:demo","node":roots[1]["key"]}),
                SOON,
            )
            .unwrap();
        assert_eq!(children[0]["label"], "Child");
        host.request(
            "view.run",
            json!({"id":"tests:demo","run":roots[1]["actions"][0]["run"]}),
            SOON,
        )
        .unwrap();
        let roots = host
            .request("view.children", json!({"id":"tests:demo"}), SOON)
            .unwrap();
        assert_eq!(roots[1]["mark"], "Failed");
        assert_eq!(roots[1]["description"], "Expected one");
        assert_eq!(roots[1]["tooltip"], "Expected one\nFound two");
        let badges = host
            .request(
                "files.decorate",
                json!({"uris":["file:///project/one.rs"]}),
                SOON,
            )
            .unwrap();
        assert_eq!(badges[0]["badge"], "M");
        host.request("deactivate", json!({}), SOON).unwrap();
        assert_eq!(
            host.request(
                "files.decorate",
                json!({"uris":["file:///project/one.rs"]}),
                SOON
            ),
            Ok(json!([]))
        );
        host.stop();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
