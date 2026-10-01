//! Debugging: breakpoints, and the sessions of one run. js-debug (Microsoft's
//! JavaScript debugger, the one VS Code uses) runs as a server Solder starts;
//! the program, its child processes and a browser's pages each become a
//! session on it, and a pause in any of them shows here.
//!
//! One store for the app: breakpoints belong to files, whichever window
//! shows them, and one run is debugged at a time.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use dap::{Connection, Incoming, StackFrame, Variable};
use futures::StreamExt;
use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, SharedString, Task};
use serde_json::{Value, json};

use crate::debug_launch::LaunchConfig;

/// js-debug's server, pinned with the SHA-256 GitHub publishes for it.
pub const JS_DEBUG: &str = "v1.140.0";
const JS_DEBUG_SHA256: &str = "27dab92937ec1ab35821ae955aac867544fe06a1b6307229049f2d789af10968";
const JS_DEBUG_SIZE: u64 = 1_249_709;

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Starting(SharedString),
    Running,
    Paused,
    Failed(SharedString),
}

impl State {
    pub fn active(&self) -> bool {
        matches!(self, State::Starting(_) | State::Running | State::Paused)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConsoleLine {
    /// `stdout`, `stderr`, `console`, or `input` for what the user typed.
    pub category: String,
    pub text: String,
}

pub struct Session {
    pub id: usize,
    pub name: String,
    pub parent: Option<usize>,
    pub conn: Arc<Connection>,
    pub ended: bool,
    _task: Task<()>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Paused {
    pub session: usize,
    pub thread: i64,
    pub reason: String,
    pub frames: Vec<StackFrame>,
    pub selected: usize,
}

impl Paused {
    pub fn frame(&self) -> Option<&StackFrame> {
        self.frames.get(self.selected)
    }
}

pub enum DebugEvent {
    /// Stopped at a place to show: the file and its 1-based line.
    Paused(PathBuf, u32),
    /// A place to open, such as where a timeline entry was made.
    Reveal(PathBuf, u32),
}

impl EventEmitter<DebugEvent> for DebugStore {}

/// How to start the adapter; tests run a stand-in.
#[derive(Clone, Debug)]
pub enum AdapterSpec {
    JsDebug,
    #[cfg(test)]
    Command {
        program: PathBuf,
        args: Vec<String>,
    },
}

pub struct DebugStore {
    /// Lines with a breakpoint, by file; `true` once a session confirmed it.
    pub breakpoints: BTreeMap<PathBuf, BTreeMap<u32, bool>>,
    pub state: State,
    pub config: Option<LaunchConfig>,
    pub sessions: Vec<Session>,
    pub paused: Option<Paused>,
    /// Scopes of the selected frame: name and variables reference.
    pub scopes: Vec<(String, i64)>,
    /// Children fetched so far, by variables reference.
    pub children: HashMap<i64, Vec<Variable>>,
    pub expanded: BTreeSet<i64>,
    pub console: Vec<ConsoleLine>,
    data_dir: PathBuf,
    adapter_spec: AdapterSpec,
    adapter: Option<dap::Adapter>,
    root: PathBuf,
    next_id: usize,
    run: u64,
    /// A page to open in the browser once the server says it listens.
    pending_browser: Option<String>,
    /// Which line each breakpoint the adapter numbered stands for, so a
    /// later `breakpoint` event can mark it verified.
    breakpoint_ids: HashMap<(usize, i64), (PathBuf, u32)>,
    pub timeline: Vec<crate::debug_timeline::Entry>,
    timeline_task: Option<Task<()>>,
}

struct GlobalDebugStore(Entity<DebugStore>);

impl Global for GlobalDebugStore {}

/// What Node prints about the debugger itself on stderr: shown red among
/// the program's output, it reads like an error.
fn inspector_notice(text: &str) -> bool {
    text == "Debugger attached."
        || text.starts_with("Waiting for the debugger to disconnect")
        || text.starts_with("Debugger listening on ")
        || text.starts_with("For help, see: https://nodejs.org")
}

/// The session that drives the browser half of a server-and-browser run.
const BROWSER: &str = "Browser";

/// Timeline entries kept; older ones go.
const TIMELINE_LIMIT: usize = 5_000;

/// Console lines kept; older ones go.
const CONSOLE_LIMIT: usize = 5_000;

impl DebugStore {
    pub fn new(data_dir: PathBuf, adapter_spec: AdapterSpec) -> Self {
        Self {
            breakpoints: BTreeMap::new(),
            state: State::Idle,
            config: None,
            sessions: Vec::new(),
            paused: None,
            scopes: Vec::new(),
            children: HashMap::new(),
            expanded: BTreeSet::new(),
            console: Vec::new(),
            data_dir,
            adapter_spec,
            adapter: None,
            root: PathBuf::new(),
            next_id: 1,
            run: 0,
            pending_browser: None,
            breakpoint_ids: HashMap::new(),
            timeline: Vec::new(),
            timeline_task: None,
        }
    }

    pub fn global(cx: &mut App) -> Entity<DebugStore> {
        if let Some(store) = cx.try_global::<GlobalDebugStore>() {
            return store.0.clone();
        }
        let dir = dirs::data_dir()
            .unwrap_or_else(crate::settings::config_dir)
            .join("Solder");
        let store = cx.new(|_| DebugStore::new(dir, AdapterSpec::JsDebug));
        cx.on_app_quit({
            let store = store.clone();
            move |cx| {
                store.update(cx, |s, cx| s.shutdown(cx));
                async {}
            }
        })
        .detach();
        cx.set_global(GlobalDebugStore(store.clone()));
        store
    }

    pub fn try_global(cx: &App) -> Option<Entity<DebugStore>> {
        cx.try_global::<GlobalDebugStore>().map(|g| g.0.clone())
    }

    #[cfg(test)]
    pub fn set_global(store: Entity<DebugStore>, cx: &mut App) {
        cx.set_global(GlobalDebugStore(store));
    }

    // ---------------------------------------------------------- breakpoints

    pub fn lines(&self, path: &Path) -> Option<&BTreeMap<u32, bool>> {
        self.breakpoints.get(path)
    }

    /// Adds or removes the breakpoint on `line` (1-based) and tells every
    /// running session.
    pub fn toggle(&mut self, path: &Path, line: u32, cx: &mut Context<Self>) {
        let lines = self.breakpoints.entry(path.to_path_buf()).or_default();
        if lines.remove(&line).is_none() {
            lines.insert(line, false);
        }
        if lines.is_empty() {
            self.breakpoints.remove(path);
        }
        let ids: Vec<usize> = self
            .sessions
            .iter()
            .filter(|s| !s.ended)
            .map(|s| s.id)
            .collect();
        for id in ids {
            self.send_breakpoints(id, path, cx);
        }
        cx.notify();
    }

    fn send_breakpoints(&mut self, session: usize, path: &Path, cx: &mut Context<Self>) {
        let Some(conn) = self.session(session).map(|s| s.conn.clone()) else {
            return;
        };
        let lines: Vec<u32> = self
            .breakpoints
            .get(path)
            .map(|l| l.keys().copied().collect())
            .unwrap_or_default();
        let answer = conn.request("setBreakpoints", dap::breakpoints_arguments(path, &lines));
        let path = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            let Ok(body) = answer.await else { return };
            this.update(cx, |this, cx| {
                let Some(lines) = this.breakpoints.get_mut(&path) else {
                    return;
                };
                for (asked, got) in lines
                    .clone()
                    .keys()
                    .zip(body["breakpoints"].as_array().into_iter().flatten())
                {
                    if got["verified"].as_bool().unwrap_or(false) {
                        lines.insert(*asked, true);
                    }
                    // js-debug answers unverified and confirms once the
                    // script loads.
                    if let Some(bp) = got["id"].as_i64() {
                        this.breakpoint_ids
                            .insert((session, bp), (path.clone(), *asked));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // -------------------------------------------------------------- running

    pub fn session_name(&self, id: usize) -> Option<&str> {
        self.session(id).map(|s| s.name.as_str())
    }

    fn session(&self, id: usize) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    fn log(&mut self, category: &str, text: impl Into<String>) {
        // One row per line: an output event may hold a whole stack trace.
        for line in text.into().lines() {
            self.console.push(ConsoleLine {
                category: category.into(),
                text: line.to_string(),
            });
        }
        if self.console.len() > CONSOLE_LIMIT {
            let extra = self.console.len() - CONSOLE_LIMIT;
            self.console.drain(..extra);
        }
    }

    /// The adapter program and its arguments, installing js-debug the
    /// first time.
    async fn adapter_command(
        spec: AdapterSpec,
        data_dir: PathBuf,
        root: PathBuf,
        this: gpui::WeakEntity<Self>,
        cx: &mut gpui::AsyncApp,
    ) -> Result<(PathBuf, Vec<String>), String> {
        match spec {
            #[cfg(test)]
            AdapterSpec::Command { program, args } => Ok((program, args)),
            AdapterSpec::JsDebug => {
                let node = cx
                    .background_executor()
                    .spawn({
                        let root = root.clone();
                        async move { crate::lsp_store::find_program("node", &root) }
                    })
                    .await
                    .ok_or("Debugging JavaScript needs Node.js; install it from nodejs.org")?;
                let dir = data_dir.join("js-debug").join(JS_DEBUG);
                let server = dir.join("js-debug/src/dapDebugServer.js");
                if !server.is_file() {
                    this.update(cx, |this, cx| {
                        this.state = State::Starting(
                            "Downloading the JavaScript debugger (1.2 MB)...".into(),
                        );
                        cx.notify();
                    })
                    .ok();
                    let asset = ai::install::Asset {
                        url: format!(
                            "https://github.com/microsoft/vscode-js-debug/releases/download/{JS_DEBUG}/js-debug-dap-{JS_DEBUG}.tar.gz"
                        ),
                        size: JS_DEBUG_SIZE,
                        sha256: JS_DEBUG_SHA256.into(),
                    };
                    let archive = data_dir.join("js-debug").join(format!("{JS_DEBUG}.tar.gz"));
                    let progress = ai::Progress::new();
                    let dest = archive.clone();
                    ai::spawn(async move { ai::install::download(&asset, &dest, &progress).await })
                        .await?;
                    let target = dir.clone();
                    cx.background_executor()
                        .spawn(async move {
                            let result = ai::install::unpack(&archive, &target);
                            let _ = std::fs::remove_file(&archive);
                            result
                        })
                        .await?;
                }
                Ok((node, vec![server.display().to_string()]))
            }
        }
    }

    /// Starts debugging `config` in `root`.
    pub fn start(&mut self, config: LaunchConfig, root: PathBuf, cx: &mut Context<Self>) {
        if self.state.active() {
            self.stop(cx);
        }
        self.run += 1;
        let run = self.run;
        self.root = root.clone();
        self.config = Some(config.clone());
        self.pending_browser = config.browser.clone();
        self.console.clear();
        self.paused = None;
        self.scopes.clear();
        self.children.clear();
        self.state = State::Starting("Starting the debugger...".into());
        cx.notify();
        self.timeline.clear();
        self.timeline_task = None;
        let spec = self.adapter_spec.clone();
        let data_dir = self.data_dir.clone();
        cx.spawn(async move |this, cx| {
            let started = async {
                let (program, args) =
                    Self::adapter_command(spec, data_dir.clone(), root.clone(), this.clone(), cx)
                        .await?;
                cx.background_executor()
                    .spawn(async move {
                        let adapter = dap::Adapter::start(&program, &args, &root)
                            .map_err(|e| e.to_string())?;
                        // Without a timeline the program still runs.
                        Ok::<_, String>((adapter, crate::debug_timeline::prepare(&data_dir).ok()))
                    })
                    .await
            }
            .await;
            this.update(cx, |this, cx| {
                if this.run != run {
                    return;
                }
                match started {
                    Ok((adapter, timeline)) => {
                        let port = adapter.port;
                        this.adapter = Some(adapter);
                        this.state = State::Running;
                        let mut request = config.request.clone();
                        if let Some((preload, log)) = timeline {
                            crate::debug_timeline::add_env(&mut request, &preload, &log);
                            this.read_timeline(log, cx);
                        }
                        let name = config.name.clone();
                        this.open_session(port, name, None, "launch", request, cx);
                    }
                    Err(e) => this.state = State::Failed(e.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reads what the program records while the run lasts, and a moment
    /// after, for what it wrote as it exited.
    fn read_timeline(&mut self, log: PathBuf, cx: &mut Context<Self>) {
        self.timeline_task = Some(cx.spawn(async move |this, cx| {
            let mut offset = 0;
            let mut pending = Vec::new();
            let mut idle_ticks = 0;
            while idle_ticks < 8 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let path = log.clone();
                let (bytes, next) = cx
                    .background_executor()
                    .spawn(async move { crate::debug_timeline::read_from(&path, offset) })
                    .await;
                offset = next;
                pending.extend_from_slice(&bytes);
                let entries = crate::debug_timeline::take_entries(&mut pending);
                let Ok(active) = this.update(cx, |this, cx| {
                    if !entries.is_empty() {
                        this.add_timeline(entries);
                        cx.notify();
                    }
                    this.state.active()
                }) else {
                    return;
                };
                idle_ticks = if active { 0 } else { idle_ticks + 1 };
            }
        }));
    }

    fn add_timeline(&mut self, entries: Vec<crate::debug_timeline::Entry>) {
        self.timeline.extend(entries);
        crate::debug_timeline::order(&mut self.timeline);
        if self.timeline.len() > TIMELINE_LIMIT {
            let extra = self.timeline.len() - TIMELINE_LIMIT;
            self.timeline.drain(..extra);
        }
    }

    /// Connects a session and runs its startup: initialize, the launch or
    /// attach request, breakpoints, configurationDone.
    fn open_session(
        &mut self,
        port: u16,
        name: String,
        parent: Option<usize>,
        kind: &str,
        arguments: Value,
        cx: &mut Context<Self>,
    ) {
        let (conn, mut incoming) = match Connection::connect(port) {
            Ok(c) => c,
            Err(e) => {
                self.log("stderr", format!("Could not connect to the debugger: {e}"));
                if parent.is_none() {
                    self.state = State::Failed(e.to_string().into());
                }
                cx.notify();
                return;
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        let run = self.run;
        let conn_events = conn.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Some(message) = incoming.next().await {
                let alive = this
                    .update(cx, |this, cx| {
                        if this.run != run {
                            return false;
                        }
                        this.handle(id, message, cx);
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
            this.update(cx, |this, cx| {
                if this.run == run {
                    this.ended(id, cx);
                }
            })
            .ok();
            drop(conn_events);
        });
        self.sessions.push(Session {
            id,
            name,
            parent,
            conn: conn.clone(),
            ended: false,
            _task: task,
        });
        let init = conn.request("initialize", dap::initialize_arguments());
        let kind = kind.to_string();
        cx.spawn(async move |this, cx| {
            if let Err(e) = init.await {
                this.update(cx, |this, cx| {
                    this.log("stderr", format!("The debugger did not start: {e}"));
                    cx.notify();
                })
                .ok();
                return;
            }
            // The answer to launch comes only after configurationDone, which
            // waits for `initialized`: do not wait for it here.
            let launched = conn.request(&kind, arguments);
            if let Err(e) = launched.await {
                this.update(cx, |this, cx| {
                    this.log("stderr", e.to_string());
                    if parent.is_none() && this.run == run {
                        this.state = State::Failed(e.to_string().into());
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn handle(&mut self, id: usize, message: Incoming, cx: &mut Context<Self>) {
        match message {
            Incoming::Request {
                seq,
                command,
                arguments,
            } => {
                let Some(conn) = self.session(id).map(|s| s.conn.clone()) else {
                    return;
                };
                if command == "startDebugging" {
                    conn.respond(seq, &command, true, Value::Null);
                    let port = self.adapter.as_ref().map(|a| a.port);
                    let kind = arguments["request"]
                        .as_str()
                        .unwrap_or("launch")
                        .to_string();
                    let config = arguments["configuration"].clone();
                    // A page target is named after its first title,
                    // usually about:blank; "Browser" stays true.
                    let name = match self.session(id) {
                        Some(s) if s.name == BROWSER => BROWSER.to_string(),
                        _ => config["name"].as_str().unwrap_or("child").to_string(),
                    };
                    if let Some(port) = port {
                        self.open_session(port, name, Some(id), &kind, config, cx);
                    }
                } else {
                    conn.respond(seq, &command, false, Value::Null);
                }
            }
            Incoming::Event { event, body } => match event.as_str() {
                "initialized" => {
                    let paths: Vec<PathBuf> = self.breakpoints.keys().cloned().collect();
                    for path in paths {
                        self.send_breakpoints(id, &path, cx);
                    }
                    if let Some(s) = self.session(id) {
                        let done = s.conn.request("configurationDone", json!({}));
                        cx.background_executor()
                            .spawn(async move { done.await.ok() })
                            .detach();
                    }
                }
                "breakpoint" => {
                    let bp = &body["breakpoint"];
                    let found = bp["id"]
                        .as_i64()
                        .and_then(|bp| self.breakpoint_ids.get(&(id, bp)).cloned());
                    if let Some((path, line)) = found
                        && let Some(slot) = self
                            .breakpoints
                            .get_mut(&path)
                            .and_then(|l| l.get_mut(&line))
                    {
                        *slot = bp["verified"].as_bool().unwrap_or(false);
                        cx.notify();
                    }
                }
                "stopped" => {
                    let thread = body["threadId"].as_i64().unwrap_or(0);
                    let reason = body["reason"].as_str().unwrap_or("pause").to_string();
                    self.load_stack(id, thread, reason, cx);
                }
                "continued" => {
                    if self.paused.as_ref().is_some_and(|p| p.session == id) {
                        self.paused = None;
                        self.scopes.clear();
                        self.children.clear();
                        self.state = State::Running;
                        cx.notify();
                    }
                }
                "output" => {
                    let category = body["category"].as_str().unwrap_or("console");
                    if category != "telemetry" {
                        let text = body["output"].as_str().unwrap_or("").trim_end_matches('\n');
                        if !text.is_empty() && !inspector_notice(text) {
                            self.open_browser_when_ready(text, cx);
                            self.log(category, text);
                            cx.notify();
                        }
                    }
                }
                "exited" => {
                    let code = body["exitCode"].as_i64().unwrap_or(0);
                    if self.session(id).is_some_and(|s| s.parent.is_none()) || code != 0 {
                        self.log("console", format!("Exited with code {code}"));
                        cx.notify();
                    }
                }
                "terminated" => self.ended(id, cx),
                _ => {}
            },
        }
    }

    /// A server-and-browser run: once the server prints its address, the
    /// page opens in a browser session of the same run.
    fn open_browser_when_ready(&mut self, output: &str, cx: &mut Context<Self>) {
        let Some(url) = self.pending_browser.clone() else {
            return;
        };
        let port = url.rsplit(':').next().unwrap_or("").trim_end_matches('/');
        if port.is_empty() || !output.contains(&format!(":{port}")) {
            return;
        }
        self.pending_browser = None;
        let Some(adapter_port) = self.adapter.as_ref().map(|a| a.port) else {
            return;
        };
        // Sources map to the app's folder, which may be below the root.
        let web_root = self
            .config
            .as_ref()
            .and_then(|c| c.request["cwd"].as_str().map(PathBuf::from))
            .unwrap_or_else(|| self.root.clone());
        let request = crate::debug_launch::browser_request(&url, &web_root);
        self.log("console", format!("Opening {url} in a browser"));
        self.open_session(adapter_port, BROWSER.into(), None, "launch", request, cx);
    }

    fn ended(&mut self, id: usize, cx: &mut Context<Self>) {
        match self.sessions.iter_mut().find(|s| s.id == id) {
            Some(s) if !s.ended => s.ended = true,
            _ => return,
        }
        if self.paused.as_ref().is_some_and(|p| p.session == id) {
            self.paused = None;
            self.scopes.clear();
        }
        // The run is over when its first session (the program) is; a
        // browser opened for it closes too. Its children end on their own
        // connections and may still be delivering the program's last
        // output, so they get a moment.
        if let Some(first) = self.sessions.first().filter(|s| s.ended).map(|s| s.id) {
            let children_live = self
                .sessions
                .iter()
                .any(|s| s.parent == Some(first) && !s.ended);
            if !children_live {
                self.stop(cx);
            } else if id == first {
                let run = self.run;
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(1))
                        .await;
                    this.update(cx, |this, cx| {
                        if this.run == run {
                            this.stop(cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn load_stack(&mut self, session: usize, thread: i64, reason: String, cx: &mut Context<Self>) {
        let Some(conn) = self.session(session).map(|s| s.conn.clone()) else {
            return;
        };
        let answer = conn.request("stackTrace", json!({ "threadId": thread, "levels": 50 }));
        cx.spawn(async move |this, cx| {
            let frames = answer
                .await
                .map(|b| dap::stack_frames(&b))
                .unwrap_or_default();
            this.update(cx, |this, cx| {
                // The first frame in the user's own code.
                let selected = frames
                    .iter()
                    .position(|f| f.path.is_some() && !f.subtle)
                    .unwrap_or(0);
                this.paused = Some(Paused {
                    session,
                    thread,
                    reason,
                    frames,
                    selected,
                });
                this.state = State::Paused;
                this.select_frame(selected, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Shows a frame of the stack: its place and its variables.
    pub fn select_frame(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(paused) = self.paused.as_mut() else {
            return;
        };
        if index >= paused.frames.len() {
            return;
        }
        paused.selected = index;
        let frame = paused.frames[index].clone();
        let session = paused.session;
        self.scopes.clear();
        self.children.clear();
        self.expanded.clear();
        if let Some(path) = frame.path.clone() {
            cx.emit(DebugEvent::Paused(path, frame.line));
        }
        cx.notify();
        let Some(conn) = self.session(session).map(|s| s.conn.clone()) else {
            return;
        };
        let answer = conn.request("scopes", json!({ "frameId": frame.id }));
        cx.spawn(async move |this, cx| {
            let Ok(body) = answer.await else { return };
            let scopes: Vec<(String, i64)> = body["scopes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| {
                    (
                        s["name"].as_str().unwrap_or_default().to_string(),
                        s["variablesReference"].as_i64().unwrap_or(0),
                    )
                })
                .collect();
            this.update(cx, |this, cx| {
                // Locals open; Globals and the like stay closed.
                let first = scopes.first().map(|s| s.1);
                this.scopes = scopes;
                if let Some(reference) = first {
                    this.toggle_expanded(reference, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Opens or closes a scope or variable, fetching its children once.
    pub fn toggle_expanded(&mut self, reference: i64, cx: &mut Context<Self>) {
        if reference == 0 {
            return;
        }
        if !self.expanded.insert(reference) {
            self.expanded.remove(&reference);
            cx.notify();
            return;
        }
        if self.children.contains_key(&reference) {
            cx.notify();
            return;
        }
        let Some(conn) = self
            .paused
            .as_ref()
            .and_then(|p| self.session(p.session))
            .map(|s| s.conn.clone())
        else {
            return;
        };
        let answer = conn.request("variables", json!({ "variablesReference": reference }));
        cx.spawn(async move |this, cx| {
            let vars = answer
                .await
                .map(|b| dap::variables(&b, "variables"))
                .unwrap_or_default();
            this.update(cx, |this, cx| {
                this.children.insert(reference, vars);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn step(&mut self, command: &str, cx: &mut Context<Self>) {
        let Some(paused) = self.paused.clone() else {
            return;
        };
        let Some(conn) = self.session(paused.session).map(|s| s.conn.clone()) else {
            return;
        };
        let answer = conn.request(command, json!({ "threadId": paused.thread }));
        cx.background_executor()
            .spawn(async move { answer.await.ok() })
            .detach();
    }

    pub fn resume(&mut self, cx: &mut Context<Self>) {
        self.step("continue", cx);
    }

    pub fn step_over(&mut self, cx: &mut Context<Self>) {
        self.step("next", cx);
    }

    pub fn step_in(&mut self, cx: &mut Context<Self>) {
        self.step("stepIn", cx);
    }

    pub fn step_out(&mut self, cx: &mut Context<Self>) {
        self.step("stepOut", cx);
    }

    /// Pauses every session that runs code.
    pub fn pause(&mut self, cx: &mut Context<Self>) {
        for s in self
            .sessions
            .iter()
            .filter(|s| !s.ended && s.parent.is_some())
        {
            let threads = s.conn.request("threads", json!({}));
            let conn = s.conn.clone();
            cx.background_executor()
                .spawn(async move {
                    let Ok(body) = threads.await else { return };
                    for t in body["threads"].as_array().into_iter().flatten() {
                        let id = t["id"].as_i64().unwrap_or(0);
                        conn.request("pause", json!({ "threadId": id })).await.ok();
                    }
                })
                .detach();
        }
    }

    /// Ends the run: the program and its children stop.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let byes: Vec<_> = self
            .sessions
            .iter()
            .filter(|s| !s.ended)
            .map(|s| {
                s.conn
                    .request("disconnect", json!({ "terminateDebuggee": true }))
            })
            .collect();
        // Kept open until the adapter has answered, so the disconnect is
        // written before the socket closes.
        let conns: Vec<Arc<Connection>> = self.sessions.drain(..).map(|s| s.conn).collect();
        self.run += 1;
        let adapter = self.adapter.take();
        self.paused = None;
        self.scopes.clear();
        self.children.clear();
        self.pending_browser = None;
        self.breakpoint_ids.clear();
        // The next run's adapter verifies them again.
        for line in self.breakpoints.values_mut().flat_map(|l| l.values_mut()) {
            *line = false;
        }
        if self.state.active() {
            self.state = State::Idle;
        }
        let timeout = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(2));
        cx.background_executor()
            .spawn(async move {
                let answered = futures::future::join_all(byes);
                futures::future::select(Box::pin(answered), timeout).await;
                for conn in &conns {
                    conn.close();
                }
                drop(adapter);
            })
            .detach();
        cx.notify();
    }

    fn shutdown(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
    }

    /// Runs an expression in the paused frame, or in the program when it
    /// runs, and shows the answer in the console.
    pub fn evaluate(&mut self, expression: String, cx: &mut Context<Self>) {
        let expression = expression.trim().to_string();
        if expression.is_empty() {
            return;
        }
        self.log("input", expression.clone());
        let (conn, frame) = match &self.paused {
            Some(p) => (
                self.session(p.session).map(|s| s.conn.clone()),
                p.frame().map(|f| f.id),
            ),
            None => (
                self.sessions
                    .iter()
                    .rev()
                    .find(|s| !s.ended && s.parent.is_some())
                    .map(|s| s.conn.clone()),
                None,
            ),
        };
        let Some(conn) = conn else {
            self.log("stderr", "Nothing is running");
            cx.notify();
            return;
        };
        let mut args = json!({ "expression": expression, "context": "repl" });
        if let Some(frame) = frame {
            args["frameId"] = frame.into();
        }
        let answer = conn.request("evaluate", args);
        cx.spawn(async move |this, cx| {
            let line = match answer.await {
                Ok(body) => ("result", body["result"].as_str().unwrap_or("").to_string()),
                Err(e) => ("stderr", e.to_string()),
            };
            this.update(cx, |this, cx| {
                this.log(line.0, line.1);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}
