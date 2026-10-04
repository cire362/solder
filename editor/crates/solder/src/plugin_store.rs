//! Plugins in the app: what is installed, what the user enabled, and the
//! running ones. One store for all windows.
//!
//! A plugin is a folder under the data folder's `plugins` with a
//! `plugin.json` and a `plugin.wasm` (or, written in JavaScript, a
//! `plugin.js`). Nothing runs until the user enables
//! it, which approves the permissions its manifest lists for exactly that
//! module: if either changes, it stays off until enabled again.
//!
//! Plugins run on their own threads (`plugin::Plugin`). What they ask for
//! arrives here: files and HTTP are answered on the plugin's thread, the
//! editor's text on the UI thread, with the plugin's thread waiting.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, AppContext, Context, Entity, Global, SharedString, Subscription, Task, WeakEntity,
};
use plugin::{
    Activity, Budget, Code, EditorText, Event, Host, HttpResponse, Manifest, Plugin, Reply,
    Request, Stats,
};
use serde::{Deserialize, Serialize};

use crate::{
    document::{Document, DocumentEvent},
    workspace::Workspace,
};

/// A file a plugin may read: 4 MiB.
const FILE_LIMIT: u64 = 4 << 20;
/// How long a plugin waits for the editor's text.
const UI_TIMEOUT: Duration = Duration::from_secs(5);

/// A plugin folder and what its manifest says.
#[derive(Clone, Debug)]
pub struct Installed {
    /// The manifest's name, or the folder's when the manifest is unreadable.
    pub name: String,
    pub dir: PathBuf,
    pub manifest: Result<Manifest, String>,
    /// SHA-256 of `plugin.wasm` or `plugin.js`; empty when both are
    /// missing.
    pub hash: String,
    /// Written in JavaScript: it has a `plugin.js`.
    pub script: bool,
}

/// What the user agreed to when enabling a plugin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Approval {
    permissions: Vec<String>,
    hash: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    enabled: HashMap<String, Approval>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PluginState {
    Disabled,
    Enabled,
    /// Enabled before, but its module or permissions are not what was
    /// approved.
    Changed,
    Invalid(String),
}

enum UiMessage {
    Status {
        plugin: String,
        text: String,
    },
    Changed,
    Request {
        request: Request,
        reply: std::sync::mpsc::SyncSender<Reply>,
    },
}

/// The editor as plugins reach it, from their threads.
struct AppHost {
    ui: mpsc::UnboundedSender<UiMessage>,
    /// The project of the window in front.
    root: Mutex<Option<PathBuf>>,
}

impl Host for AppHost {
    fn request(&self, plugin: &str, request: Request) -> Reply {
        match request {
            Request::Status { text } => {
                let _ = self.ui.unbounded_send(UiMessage::Status {
                    plugin: plugin.to_string(),
                    text,
                });
                Ok(serde_json::Value::Null)
            }
            Request::ReadFile { path } => {
                let root = self.root.lock().unwrap().clone();
                let root = root.ok_or("No project is open")?;
                read_project_file(&root, &path).map(Into::into)
            }
            Request::Http(http) => {
                // Redirects are handed back: the next host is checked too.
                let sent = rest::send_direct(rest::Request {
                    name: String::new(),
                    method: http.method,
                    url: http.url,
                    headers: http.headers,
                    body: http.body,
                });
                let response = futures::executor::block_on(sent)?;
                serde_json::to_value(HttpResponse {
                    status: response.status,
                    body: String::from_utf8_lossy(&response.body).into_owned(),
                    headers: response.headers,
                })
                .map_err(|e| e.to_string())
            }
            request @ (Request::EditorText | Request::Edit { .. }) => {
                let (reply, answer) = std::sync::mpsc::sync_channel(1);
                self.ui
                    .unbounded_send(UiMessage::Request { request, reply })
                    .map_err(|_| "The editor is closing")?;
                answer
                    .recv_timeout(UI_TIMEOUT)
                    .unwrap_or_else(|_| Err("The editor did not answer".into()))
            }
            Request::Log { .. } => Ok(serde_json::Value::Null),
        }
    }

    fn changed(&self, _: &str) {
        let _ = self.ui.unbounded_send(UiMessage::Changed);
    }
}

/// A file of the project by its path from the root: inside the project,
/// not `.env`, not kept from AI by `.solderignore`, and text.
fn read_project_file(root: &Path, path: &str) -> Result<String, String> {
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("A path from the project's root, without ..".into());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    // Resolved, so a link out of the project is caught.
    let file = root
        .join(relative)
        .canonicalize()
        .map_err(|_| format!("No file {path}"))?;
    if !file.starts_with(&root) {
        return Err("The file is outside the project".into());
    }
    if !crate::ai_context::allows_path(&root, &file)? {
        return Err("The file is kept private (.env or .solderignore)".into());
    }
    let size = std::fs::metadata(&file).map_err(|e| e.to_string())?.len();
    if size > FILE_LIMIT {
        return Err("The file is larger than 4 MB".into());
    }
    std::fs::read_to_string(&file).map_err(|_| "The file is not text".into())
}

/// As plugins see a path: from the project's root when inside it.
fn shown_path(root: Option<&Path>, path: &Path) -> String {
    root.and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub struct PluginStore {
    /// Where plugins are installed, and the file that keeps what is enabled.
    pub dir: PathBuf,
    settings: PathBuf,
    pub installed: Vec<Installed>,
    pub loaded: bool,
    enabled: HashMap<String, Approval>,
    running: HashMap<String, Plugin>,
    /// Each plugin's text in the status bar.
    pub status: BTreeMap<String, SharedString>,
    activity: Arc<Activity>,
    host: Arc<AppHost>,
    budget: Budget,
    /// The window in front, whose editor plugins read and change.
    workspace: Option<WeakEntity<Workspace>>,
    front: Option<PathBuf>,
    /// Whether any running plugin asked for `open` or `change`.
    wants_open: bool,
    wants_change: bool,
    scans: u64,
    _pump: Task<()>,
    _observe: Option<Subscription>,
}

struct GlobalPluginStore(Entity<PluginStore>);

impl Global for GlobalPluginStore {}

impl PluginStore {
    pub fn new(data_dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let (ui, mut messages) = mpsc::unbounded();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(message) = messages.next().await {
                if this
                    .update(cx, |this, cx| this.on_message(message, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            dir: data_dir.join("plugins"),
            settings: data_dir.join("plugins.json"),
            installed: Vec::new(),
            loaded: false,
            enabled: HashMap::new(),
            running: HashMap::new(),
            status: BTreeMap::new(),
            activity: Arc::new(Activity::default()),
            host: Arc::new(AppHost {
                ui,
                root: Mutex::new(None),
            }),
            budget: Budget::default(),
            workspace: None,
            front: None,
            wants_open: false,
            wants_change: false,
            scans: 0,
            _pump: pump,
            _observe: None,
        }
    }

    /// The app's store; the first call reads what is installed, off the UI
    /// thread.
    pub fn global(cx: &mut App) -> Entity<PluginStore> {
        if let Some(store) = cx.try_global::<GlobalPluginStore>() {
            return store.0.clone();
        }
        // Tests never see the user's own plugins.
        #[cfg(test)]
        let dir = db::testing::dir("no-plugins");
        #[cfg(not(test))]
        let dir = dirs::data_dir()
            .unwrap_or_else(crate::settings::config_dir)
            .join("Solder");
        let store = cx.new(|cx| PluginStore::new(dir, cx));
        cx.set_global(GlobalPluginStore(store.clone()));
        store.update(cx, |s, cx| s.scan(cx));
        store
    }

    pub fn try_global(cx: &App) -> Option<Entity<PluginStore>> {
        cx.try_global::<GlobalPluginStore>().map(|g| g.0.clone())
    }

    #[cfg(test)]
    pub fn set_global(store: Entity<PluginStore>, cx: &mut App) {
        cx.set_global(GlobalPluginStore(store));
    }

    #[cfg(test)]
    pub fn set_budget(&mut self, budget: Budget) {
        self.budget = budget;
    }

    // ------------------------------------------------------------ installed

    /// Reads the plugins folder again and starts or stops plugins to match
    /// what is enabled.
    pub fn scan(&mut self, cx: &mut Context<Self>) {
        self.scans += 1;
        let scan = self.scans;
        let (dir, settings, first) = (self.dir.clone(), self.settings.clone(), !self.loaded);
        cx.spawn(async move |this, cx| {
            let (installed, saved) = cx
                .background_executor()
                .spawn(async move {
                    let saved = first.then(|| {
                        std::fs::read_to_string(&settings)
                            .ok()
                            .and_then(|text| serde_json::from_str::<Saved>(&text).ok())
                            .unwrap_or_default()
                    });
                    (read_installed(&dir), saved)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.scans != scan {
                    return;
                }
                if let Some(saved) = saved {
                    this.enabled = saved.enabled;
                }
                this.installed = installed;
                this.loaded = true;
                let names: Vec<String> = this.installed.iter().map(|p| p.name.clone()).collect();
                // Gone or no longer what was approved: stop.
                let stale: Vec<String> = this
                    .running
                    .keys()
                    .filter(|name| this.state(name) != PluginState::Enabled)
                    .cloned()
                    .collect();
                for name in stale {
                    this.stop(&name, cx);
                }
                for name in names {
                    if this.state(&name) == PluginState::Enabled
                        && !this.running.contains_key(&name)
                    {
                        this.start(&name, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn find(&self, name: &str) -> Option<&Installed> {
        self.installed.iter().find(|p| p.name == name)
    }

    pub fn state(&self, name: &str) -> PluginState {
        let Some(installed) = self.find(name) else {
            return PluginState::Disabled;
        };
        let manifest = match &installed.manifest {
            Ok(manifest) => manifest,
            Err(e) => return PluginState::Invalid(e.clone()),
        };
        if installed.hash.is_empty() {
            return PluginState::Invalid(format!(
                "Neither {} nor {} is there",
                plugin::MODULE,
                plugin::SCRIPT
            ));
        }
        match self.enabled.get(name) {
            None => PluginState::Disabled,
            Some(approval) if *approval == approval_of(manifest, &installed.hash) => {
                PluginState::Enabled
            }
            Some(_) => PluginState::Changed,
        }
    }

    pub fn stats(&self, name: &str) -> Option<Stats> {
        self.running.get(name).map(Plugin::stats)
    }

    /// Any running plugin marked slow, for the status bar.
    pub fn slow(&self) -> Vec<&str> {
        let mut slow: Vec<&str> = self
            .running
            .iter()
            .filter(|(_, p)| p.stats().slow())
            .map(|(name, _)| name.as_str())
            .collect();
        slow.sort();
        slow
    }

    // --------------------------------------------------------------- enable

    /// Approves the plugin's permissions as its manifest lists them now, for
    /// the module as it is now, and starts it.
    pub fn enable(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(installed) = self.find(name) else {
            return;
        };
        let Ok(manifest) = &installed.manifest else {
            return;
        };
        let approval = approval_of(manifest, &installed.hash);
        self.enabled.insert(name.to_string(), approval);
        self.save(cx);
        self.stop(name, cx);
        self.start(name, cx);
        cx.notify();
    }

    pub fn disable(&mut self, name: &str, cx: &mut Context<Self>) {
        self.enabled.remove(name);
        self.save(cx);
        self.stop(name, cx);
        cx.notify();
    }

    fn save(&self, cx: &mut Context<Self>) {
        let saved = Saved {
            enabled: self.enabled.clone(),
        };
        let path = self.settings.clone();
        cx.background_executor()
            .spawn(async move {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Ok(text) = serde_json::to_string_pretty(&saved) {
                    let _ = std::fs::write(path, text);
                }
            })
            .detach();
    }

    /// Reads the module and runs it if it is still what was approved.
    fn start(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(installed) = self.find(name).cloned() else {
            return;
        };
        let (Ok(manifest), Some(approval)) = (installed.manifest, self.enabled.get(name).cloned())
        else {
            return;
        };
        let name = name.to_string();
        cx.spawn(async move |this, cx| {
            let dir = installed.dir.clone();
            let code = cx
                .background_executor()
                .spawn(async move { read_code(&dir) })
                .await;
            this.update(cx, |this, cx| {
                let Some((code, hash)) = code else { return };
                // Hashed again: the bytes that run are the bytes approved.
                if approval != approval_of(&manifest, &hash) {
                    // Replaced since the folder was read: show it as changed.
                    return this.scan(cx);
                }
                if this.enabled.get(&name) != Some(&approval) || this.running.contains_key(&name) {
                    return;
                }
                let plugin = Plugin::start(
                    manifest,
                    code,
                    this.host.clone(),
                    this.activity.clone(),
                    this.budget,
                );
                plugin.send(Event::Activate);
                if let Some(event) = this.open_event(cx) {
                    plugin.send(event);
                }
                this.running.insert(name, plugin);
                this.refresh_wants();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn stop(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.running.remove(name).is_some() {
            self.status.remove(name);
            self.refresh_wants();
            cx.notify();
        }
    }

    fn refresh_wants(&mut self) {
        let wants = |event: &str| {
            self.running.keys().any(|name| {
                self.find(name)
                    .and_then(|p| p.manifest.as_ref().ok())
                    .is_some_and(|m| m.events.iter().any(|e| e == event))
            })
        };
        (self.wants_open, self.wants_change) = (wants("open"), wants("change") || wants("save"));
    }

    // -------------------------------------------------------------- commands

    /// Commands of running plugins: plugin, id, title.
    pub fn commands(&self) -> Vec<(String, String, String)> {
        let mut commands: Vec<_> = self
            .running
            .keys()
            .filter_map(|name| self.find(name)?.manifest.as_ref().ok())
            .flat_map(|m| {
                m.commands
                    .iter()
                    .map(|c| (m.name.clone(), c.id.clone(), c.title.clone()))
            })
            .collect();
        commands.sort_by(|a, b| a.2.cmp(&b.2));
        commands
    }

    pub fn run_command(&self, plugin: &str, id: &str) {
        if let Some(plugin) = self.running.get(plugin) {
            plugin.send(Event::Command { id: id.to_string() });
        }
    }

    // ---------------------------------------------------------------- events

    fn emit(&self, event: Event) {
        for plugin in self.running.values() {
            plugin.send(event.clone());
        }
    }

    /// The window in front: its editor is the one plugins read, its project
    /// the one they read files of.
    ///
    /// Called from the workspace's own update, so it is not read here.
    pub fn set_workspace(
        &mut self,
        workspace: WeakEntity<Workspace>,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.as_ref() == Some(&workspace) {
            return;
        }
        *self.host.root.lock().unwrap() = Some(root);
        // The workspace notifies on every change; only the file in front
        // matters here.
        self._observe = workspace
            .upgrade()
            .map(|w| cx.observe(&w, |this, _, cx| this.front_changed(cx)));
        self.workspace = Some(workspace);
        self.front = None;
        cx.spawn(async move |this, cx| {
            this.update(cx, |this, cx| this.front_changed(cx)).ok();
        })
        .detach();
    }

    fn front_editor(&self, cx: &App) -> Option<Entity<crate::editor::Editor>> {
        let workspace = self.workspace.as_ref()?.upgrade()?;
        workspace.read(cx).active_editor().cloned()
    }

    fn root(&self) -> Option<PathBuf> {
        self.host.root.lock().unwrap().clone()
    }

    fn open_event(&self, cx: &App) -> Option<Event> {
        let editor = self.front_editor(cx)?;
        let editor = editor.read(cx);
        Some(Event::Open {
            path: shown_path(self.root().as_deref(), editor.path(cx)?),
            language: editor.doc(cx).language_name().map(Into::into),
        })
    }

    /// Tells plugins when another file comes to the front. Runs on every
    /// change of the workspace, so it compares before it does anything.
    fn front_changed(&mut self, cx: &mut Context<Self>) {
        let editor = self.front_editor(cx);
        let path = editor.as_ref().and_then(|e| e.read(cx).path(cx));
        if path == self.front.as_deref() {
            return;
        }
        self.front = path.map(Path::to_path_buf);
        if self.wants_open
            && let Some(event) = self.open_event(cx)
        {
            self.emit(event);
        }
    }

    /// Follows a document's edits and saves.
    pub fn register(document: &Entity<Document>, cx: &mut App) {
        let Some(store) = Self::try_global(cx) else {
            return;
        };
        store.update(cx, |_, cx| {
            cx.subscribe(document, |this, document, event, cx| {
                // The typing path: nothing unless a plugin listens.
                if !this.wants_change {
                    return;
                }
                let edited = match event {
                    DocumentEvent::Edited { .. } => true,
                    DocumentEvent::Saved => false,
                    _ => return,
                };
                let Some(path) = document.read(cx).path() else {
                    return;
                };
                let path = shown_path(this.root().as_deref(), path);
                if edited {
                    this.activity.touch();
                    this.emit(Event::Change { path });
                } else {
                    this.emit(Event::Save { path });
                }
            })
            .detach();
        });
    }

    // ------------------------------------------------------------- requests

    fn on_message(&mut self, message: UiMessage, cx: &mut Context<Self>) {
        match message {
            UiMessage::Changed => cx.notify(),
            UiMessage::Status { plugin, text } => {
                // A plugin stopped meanwhile says nothing.
                if !self.running.contains_key(&plugin) {
                    return;
                }
                let text: String = text.chars().filter(|c| !c.is_control()).take(80).collect();
                if text.is_empty() {
                    self.status.remove(&plugin);
                } else {
                    self.status.insert(plugin, text.into());
                }
                cx.notify();
            }
            UiMessage::Request { request, reply } => {
                let _ = reply.send(self.answer(request, cx));
            }
        }
    }

    fn answer(&mut self, request: Request, cx: &mut Context<Self>) -> Reply {
        let editor = self.front_editor(cx).ok_or("No file is open")?;
        let root = self.root();
        match request {
            Request::EditorText => {
                let editor = editor.read(cx);
                let selection = &editor.selections[editor.newest];
                let text = EditorText {
                    path: editor.path(cx).map(|p| shown_path(root.as_deref(), p)),
                    language: editor.doc(cx).language_name().map(Into::into),
                    text: editor.text(cx),
                    selection_start: selection.anchor.min(selection.head),
                    selection_end: selection.anchor.max(selection.head),
                };
                serde_json::to_value(text).map_err(|e| e.to_string())
            }
            Request::Edit {
                path,
                start,
                end,
                text,
            } => {
                let current = editor.read(cx);
                let front = current.path(cx).map(|p| shown_path(root.as_deref(), p));
                if front.as_deref() != Some(path.as_str()) {
                    return Err(format!("{path} is not the file in front"));
                }
                if current.doc(cx).is_read_only() {
                    return Err("The file is read-only".into());
                }
                let old = current.text(cx);
                if start > end
                    || end > old.len()
                    || !old.is_char_boundary(start)
                    || !old.is_char_boundary(end)
                {
                    return Err("The range is not inside the text".into());
                }
                editor.update(cx, |e, cx| e.replace_ranges(vec![(start..end, text)], cx));
                Ok(serde_json::Value::Null)
            }
            _ => Err("Not answered by the editor".into()),
        }
    }
}

fn approval_of(manifest: &Manifest, hash: &str) -> Approval {
    Approval {
        permissions: manifest.permissions.iter().map(|p| p.name()).collect(),
        hash: hash.to_string(),
    }
}

/// What the plugin in `dir` runs, with its SHA-256: `plugin.wasm`, or
/// `plugin.js` when there is no module. Blocking.
fn read_code(dir: &Path) -> Option<(Code, String)> {
    if let Ok(wasm) = std::fs::read(dir.join(plugin::MODULE)) {
        let hash = ai::install::sha256_hex(&wasm);
        return Some((Code::Module(wasm), hash));
    }
    let source = std::fs::read_to_string(dir.join(plugin::SCRIPT)).ok()?;
    let hash = ai::install::sha256_hex(source.as_bytes());
    Some((Code::Script(source), hash))
}

/// Every folder under `dir`, with its manifest and module hash; blocking.
fn read_installed(dir: &Path) -> Vec<Installed> {
    // There for "Open folder" to show, before the first plugin.
    let _ = std::fs::create_dir_all(dir);
    let mut installed: Vec<Installed> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let dir = entry.path();
            let manifest = Manifest::load(&dir);
            let name = match &manifest {
                Ok(manifest) => manifest.name.clone(),
                Err(_) => entry.file_name().to_string_lossy().into_owned(),
            };
            let (script, hash) = match read_code(&dir) {
                Some((code, hash)) => (matches!(code, Code::Script(_)), hash),
                None => (false, String::new()),
            };
            Installed {
                name,
                dir,
                manifest,
                hash,
                script,
            }
        })
        .collect();
    installed.sort_by(|a, b| a.name.cmp(&b.name));
    // Two folders with one name: the first is the plugin.
    installed.dedup_by(|b, a| a.name == b.name);
    installed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_read_project_files_only_and_not_private_ones() {
        let root = db::testing::dir("plugin-files").canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.txt"), "hello").unwrap();
        std::fs::write(root.join(".env"), "SECRET=1").unwrap();
        std::fs::write(root.join("notes.md"), "private").unwrap();
        std::fs::write(root.join(".solderignore"), "notes.md\n").unwrap();
        std::fs::write(root.join("bin.dat"), [0xff, 0xfe, 0x00]).unwrap();
        let outside = db::testing::dir("plugin-files-outside");
        std::fs::write(outside.join("secret.txt"), "outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();

        assert_eq!(read_project_file(&root, "src/a.txt").unwrap(), "hello");
        for (path, why) in [
            ("../plugin-files-outside/secret.txt", "without .."),
            ("/etc/passwd", "without .."),
            ("src/../.env", "without .."),
            (".env", "kept private"),
            ("notes.md", "kept private"),
            ("missing.txt", "No file"),
            ("bin.dat", "not text"),
            #[cfg(unix)]
            ("link.txt", "outside the project"),
        ] {
            let error = read_project_file(&root, path).unwrap_err();
            assert!(error.contains(why), "{path}: {error}");
        }
        assert_eq!(
            shown_path(Some(&root), &root.join("src/a.txt")),
            "src/a.txt"
        );
        assert_eq!(shown_path(Some(&root), Path::new("/x/y")), "/x/y");
    }
}
