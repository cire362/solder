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
        let host = VsHost::start(
            &node.to_string_lossy(),
            &script,
            &dir.join("ext"),
            &dir.join("storage"),
            &[],
            move |told| {
                let _ = tx.send(told);
            },
        )
        .unwrap();
        Some((host, rx, dir))
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
              const tree = vscode.window.createTreeView('demo', {});
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
        heard(&told, |told| {
            *told == Told::Missing("window.createTreeView".into())
        });
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

    /// Plays the editor for a host: answers what it asks with `answer`,
    /// and keeps what it says, each as its name and what came with it.
    fn editor(
        host: &Arc<VsHost>,
        told: mpsc::Receiver<Told>,
        answer: impl Fn(&str, &Value) -> Answer + Send + 'static,
    ) -> Arc<Mutex<Vec<(String, Value)>>> {
        let said = Arc::new(Mutex::new(Vec::new()));
        let (host, kept) = (Arc::downgrade(host), said.clone());
        std::thread::spawn(move || {
            for told in told {
                match told {
                    Told::Asked { id, method, params } => {
                        let Some(host) = host.upgrade() else { break };
                        host.answer(id, answer(&method, &params));
                        lock(&kept).push((method, params));
                    }
                    Told::Said { method, params } => lock(&kept).push((method, params)),
                    _ => {}
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
        let Some((host, told, dir)) = hosted("vscode-api", code) else {
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
        let said = editor(&host, told, move |method, params| match method {
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
}
