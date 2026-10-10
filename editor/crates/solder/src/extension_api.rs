//! What the code of VS Code extensions sees of the editor, and what it asks
//! of it.
//!
//! An extension reads the folders, the settings and the open documents
//! without waiting, so its host keeps a copy of them: the store says
//! everything once, when a host comes up, and then each change. None of
//! this is done while no extension's code runs.
//!
//! What an extension asks for is either the store's to do (its status bar
//! items, its output, the settings file) or needs a window: a list to pick
//! from, a file to show, an edit to make. Those wait in a queue for the
//! workspace in front, which takes them one at a time.

use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use extension::vscode::VsHost;
use gpui::{App, Context, Entity, EntityId, EventEmitter, Subscription, Task, WeakEntity};
use serde_json::{Value, json};

use crate::{
    document::{Document, DocumentEvent},
    extension_store::ExtensionStore,
    lsp_store::to_position,
    settings::Settings,
    theme::ActiveTheme,
    workspace::Workspace,
};

/// How much of what an extension wrote to one output channel is kept.
const OUTPUT: usize = 256 * 1024;
/// How long a message that asks nothing stays in the status bar.
const NOTE: Duration = Duration::from_secs(8);

/// An item an extension put in the status bar.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusItem {
    pub text: String,
    pub tooltip: Option<String>,
    /// The command a click runs, with what it is given.
    pub command: Option<(String, Value)>,
    pub visible: bool,
    pub right: bool,
    pub priority: f64,
}

/// How loud something an extension says is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Warning,
    Error,
}

/// One thing extensions have in the status bar.
#[derive(Clone, Debug, PartialEq)]
pub struct BarText {
    pub text: String,
    pub tone: Tone,
    pub command: Option<(String, Value)>,
}

/// Where the answer to something an extension asked goes. Dropped without
/// one, the extension hears that nothing was chosen, as when a list is
/// closed with Escape: it never waits for an answer that cannot come.
pub struct Reply {
    host: Arc<VsHost>,
    id: Option<Value>,
}

impl Reply {
    pub fn send(mut self, answer: Result<Value, String>) {
        if let Some(id) = self.id.take() {
            self.host.answer(id, answer);
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.host.answer(id, Ok(Value::Null));
        }
    }
}

/// Runs a command of an extension's code: what a click on its status bar
/// item does.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunExtensionCommand {
    pub command: String,
    /// What the command is given, as a list.
    pub args: Value,
    /// The condition a key runs it under, as the extension wrote it:
    /// where it does not hold, the key is somebody else's.
    pub when: Option<String>,
}

/// The command behind "Open with": Solder's own, given the extension, the
/// kind of editor and the file.
const OPEN_WITH: &str = "solder.openWith";

/// A command of an extension as a list offers it: what it is called, and
/// what choosing it does.
#[derive(Clone, Debug, PartialEq)]
pub struct Offered {
    pub title: String,
    pub action: RunExtensionCommand,
    /// What its key runs, if it has one: the same command under the
    /// key's condition, which a list uses to find the key and show it.
    pub keyed: Option<RunExtensionCommand>,
}

/// The part of the window a key of an extension works in, read from its
/// condition: one for when the editor has the keyboard is bound in the
/// editor, so that the same key elsewhere stays what it was.
fn key_context(when: Option<&str>) -> Option<&'static str> {
    let when = when?;
    let wants = |fact: &str| {
        when.match_indices(fact)
            .any(|(at, _)| !when[..at].trim_end().ends_with('!'))
    };
    if ["editorTextFocus", "editorFocus", "textInputFocus"]
        .into_iter()
        .any(wants)
    {
        Some("Editor && mode == full")
    } else if wants("terminalFocus") {
        Some("Terminal")
    } else {
        None
    }
}

// Read by hand, as the other actions with a name in them are: no schema
// is needed for a string and what goes with it.
impl gpui::Action for RunExtensionCommand {
    fn name(&self) -> &'static str {
        Self::name_for_type()
    }
    fn name_for_type() -> &'static str {
        "workspace::RunExtensionCommand"
    }
    fn boxed_clone(&self) -> Box<dyn gpui::Action> {
        Box::new(self.clone())
    }
    fn partial_eq(&self, other: &dyn gpui::Action) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
    fn build(value: Value) -> gpui::Result<Box<dyn gpui::Action>> {
        Ok(Box::new(serde_json::from_value::<Self>(value)?))
    }
}
gpui::register_action!(RunExtensionCommand);

/// A line of a list an extension shows to pick from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PickRow {
    pub label: String,
    pub description: String,
    pub detail: String,
}

/// What an extension asks that needs a window.
pub enum Ask {
    /// A list to pick one of: the answer is its number.
    Pick {
        title: String,
        rows: Vec<PickRow>,
        reply: Reply,
    },
    /// A line to type: the answer is the text.
    Input {
        title: String,
        value: String,
        reply: Reply,
    },
    /// A file to bring to the front, at a place in it.
    Show {
        path: PathBuf,
        at: Option<lsp::types::Range>,
        reply: Option<Reply>,
    },
    Edit {
        edit: lsp::types::WorkspaceEdit,
        reply: Reply,
    },
    /// A file to save, or every changed one.
    Save {
        path: Option<PathBuf>,
        reply: Reply,
    },
    /// Every changed file saved, with nobody waiting to hear of it.
    SaveAll,
    /// Something about a terminal of an extension's: by the extension and
    /// the number it gave the terminal.
    Terminal {
        extension: String,
        terminal: u64,
        what: TerminalAsk,
    },
    /// What an extension wrote to an output channel, to read.
    Output {
        title: String,
        text: String,
    },
    Webview {
        key: crate::webview::Key,
    },
}

/// What an extension does with a terminal of its own.
pub enum TerminalAsk {
    /// A terminal in the dock, running `program` (the user's shell where
    /// it names none). `keep` leaves its tab when the program ends, for
    /// the output of a task to be read.
    Create {
        name: Option<String>,
        program: Option<String>,
        args: Vec<String>,
        cwd: Option<PathBuf>,
        env: std::collections::HashMap<String, String>,
        keep: bool,
        show: bool,
    },
    /// Text typed into it.
    Send(String),
    Show,
    Dispose,
}

/// A task of an extension's as a list offers it.
#[derive(Clone, Debug, PartialEq)]
pub struct OfferedTask {
    pub extension: String,
    /// Its place in what the extension last listed.
    pub index: usize,
    pub name: String,
    /// What it is a task of, as the extension says: `npm`, `cargo`.
    pub source: String,
}

impl Ask {
    /// Whether it takes the keyboard until it is answered.
    fn is_modal(&self) -> bool {
        matches!(self, Ask::Pick { .. } | Ask::Input { .. })
    }
}

pub enum ExtensionEvent {
    /// Something waits in the queue for the window in front.
    Asked,
    /// What extensions show in the status bar changed.
    Bar,
    Views,
    Files,
    Webviews,
}

impl EventEmitter<ExtensionEvent> for ExtensionStore {}

/// A document the store follows, and the path its extensions know it by.
struct Followed {
    path: Option<PathBuf>,
    _events: Subscription,
    _gone: Subscription,
}

/// The editor's side of the API: what was said to the hosts, and what
/// extensions put on screen.
#[derive(Default)]
pub struct Api {
    pub(crate) webviews: BTreeMap<crate::webview::Key, crate::webview::Model>,
    pub(crate) web_posts: BTreeMap<crate::webview::Key, VecDeque<(Value, Reply)>>,
    pub(crate) files: crate::extension_decorations::Files,
    pub(crate) views: BTreeMap<(String, String), crate::extension_views::View>,
    /// The windows' workspaces and their folders, the one in front first.
    workspaces: Vec<(WeakEntity<Workspace>, PathBuf)>,
    _front: Option<Subscription>,
    followed: std::collections::HashMap<EntityId, Followed>,
    /// What the hosts were last told is in front, and of the settings.
    active: Option<Value>,
    configuration: Option<Value>,
    folders: Vec<PathBuf>,
    /// By extension and the number it gave the item.
    status: BTreeMap<(String, u64), StatusItem>,
    progress: BTreeMap<(String, u64), String>,
    /// The last message that asked nothing, until it has been read.
    note: Option<(Tone, String)>,
    notes: usize,
    /// What extensions wrote, by extension and channel.
    output: BTreeMap<(String, String), String>,
    asks: VecDeque<Ask>,
    /// What extensions said of themselves for their conditions to read
    /// (`setContext`).
    contexts: std::collections::HashMap<String, Value>,
    /// The keys extensions bind, as they were last given to the keymap.
    keys: String,
}

fn uri(path: &Path) -> String {
    lsp::path_to_uri(path).to_string()
}

fn path_of(uri: &Value) -> Option<PathBuf> {
    let uri: lsp::types::Uri = uri.as_str()?.parse().ok()?;
    lsp::uri_to_path(&uri)
}

/// What VS Code calls the document's language.
pub(crate) fn language_id(document: &Document) -> String {
    let id = document.language_id();
    if id != "plaintext" {
        return id.to_string();
    }
    // A language an extension brought: the first of its names that reads
    // as an id.
    let plain = |id: &String| {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_+#".contains(c))
    };
    document
        .language_ids_at(0)
        .into_iter()
        .find(plain)
        .unwrap_or_else(|| id.to_string())
}

fn opened(document: &Document, path: &Path) -> Value {
    json!({
        "uri": uri(path),
        "languageId": language_id(document),
        "version": document.version(),
        "text": document.text().rope().to_string(),
        "dirty": document.is_dirty(),
    })
}

/// Text of the status bar without the pictures VS Code draws in it
/// (`$(check) Done`): Solder has no font of them.
fn without_icons(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("$(") {
        let Some(end) = rest[start..].find(')') else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl ExtensionStore {
    fn hosts(&self) -> impl Iterator<Item = &Arc<VsHost>> {
        self.code.values().filter_map(|code| code.host.as_ref())
    }

    /// Says something to every extension whose code runs.
    fn tell(&self, method: &str, params: Value) {
        for host in self.hosts() {
            host.notify(method, params.clone());
        }
    }

    /// The window in front: its file is the one extensions see as active,
    /// and it is where they ask the user.
    ///
    /// Called from the workspace's own update, so it is not read here.
    pub fn set_front(
        &mut self,
        workspace: WeakEntity<Workspace>,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let api = &mut self.api;
        api.workspaces
            .retain(|(known, _)| known.upgrade().is_some() && *known != workspace);
        api.workspaces.insert(0, (workspace.clone(), root));
        api._front = workspace
            .upgrade()
            .map(|front| cx.observe(&front, |this, _, cx| this.front_changed(cx)));
        cx.spawn(async move |this, cx| {
            this.update(cx, |this, cx| {
                this.folders_changed();
                this.front_changed(cx);
                // What waited for a window has one now.
                if !this.api.asks.is_empty() {
                    cx.emit(ExtensionEvent::Asked);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn is_front(&self, workspace: &Entity<Workspace>) -> bool {
        self.api
            .workspaces
            .first()
            .is_some_and(|(front, _)| front.entity_id() == workspace.entity_id())
    }

    /// The next thing an extension asked of the window in front. One that
    /// takes the keyboard is given only when `modal` says nothing else has
    /// it; the others do not wait behind it.
    pub fn take_ask(&mut self, modal: bool) -> Option<Ask> {
        let next = self
            .api
            .asks
            .iter()
            .position(|ask| modal || !ask.is_modal())?;
        self.api.asks.remove(next)
    }

    pub(crate) fn ask(&mut self, ask: Ask, cx: &mut Context<Self>) {
        self.api.asks.push_back(ask);
        cx.emit(ExtensionEvent::Asked);
    }

    /// The commands of VS Code itself that Solder does: the few that
    /// extensions call most. `None` for any other.
    pub(crate) fn own_command(
        &mut self,
        command: &str,
        args: &Value,
        cx: &mut Context<Self>,
    ) -> Option<Result<Value, String>> {
        match command {
            // Extensions set these for the conditions of their menus and
            // keys, which read them.
            "setContext" => {
                if let Some(name) = args[0].as_str() {
                    self.api.contexts.insert(name.to_string(), args[1].clone());
                }
                Some(Ok(Value::Null))
            }
            "vscode.open" => {
                let target = &args[0];
                let target = match &target["$uri"] {
                    Value::Null => target,
                    uri => uri,
                };
                let Some(path) = path_of(target) else {
                    return Some(Err("Only files can be opened".into()));
                };
                let (at, reply) = (None, None);
                self.ask(Ask::Show { path, at, reply }, cx);
                Some(Ok(Value::Null))
            }
            // A file opened with an editor of an extension's: the code is
            // started for it, and puts a page in a tab.
            OPEN_WITH => {
                let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
                let (owner, view_type) = (text(&args[0]), text(&args[1]));
                let file = match &args[2]["$uri"] {
                    Value::Null => args[2].clone(),
                    uri => uri.clone(),
                };
                let params = json!({ "viewType": view_type, "uri": file });
                let opening = self.ask_host(&owner, "customEditor.open", params, cx);
                cx.spawn(async move |this, cx| {
                    if let Err(error) = opening.await {
                        this.update(cx, |this, cx| this.report(error, cx)).ok();
                    }
                })
                .detach();
                Some(Ok(Value::Null))
            }
            "workbench.action.files.saveAll" => {
                self.api.asks.push_back(Ask::SaveAll);
                cx.emit(ExtensionEvent::Asked);
                Some(Ok(Value::Null))
            }
            _ => None,
        }
    }

    fn folders_changed(&mut self) {
        let api = &mut self.api;
        api.workspaces
            .retain(|(workspace, _)| workspace.upgrade().is_some());
        let mut folders: Vec<PathBuf> = Vec::new();
        for (_, root) in &api.workspaces {
            if !folders.contains(root) {
                folders.push(root.clone());
            }
        }
        if folders != api.folders {
            api.folders = folders;
            self.tell("folders", json!({ "folders": self.api.folders }));
        }
    }

    /// The file in front and where its cursors are, as hosts are told.
    fn active(&self, cx: &App) -> Value {
        let editor = self
            .api
            .workspaces
            .first()
            .and_then(|(workspace, _)| workspace.upgrade())
            .and_then(|workspace| workspace.read(cx).active_editor().cloned());
        let Some(editor) = editor else {
            return json!({ "uri": null });
        };
        let editor = editor.read(cx);
        let document = editor.doc(cx);
        let Some(path) = document.path() else {
            return json!({ "uri": null });
        };
        let buffer = document.text();
        let at = |offset: usize| {
            let position = to_position(buffer, offset, lsp::Encoding::Utf16);
            json!({ "line": position.line, "character": position.character })
        };
        let selections: Vec<Value> = editor
            .selections
            .iter()
            .map(
                |selection| json!({ "anchor": at(selection.anchor), "active": at(selection.head) }),
            )
            .collect();
        json!({ "uri": uri(path), "selections": selections })
    }

    /// Another file came to the front or a cursor moved. The workspace
    /// says so on every change of its own, so this compares first, and
    /// does nothing at all while no extension's code runs.
    fn front_changed(&mut self, cx: &mut Context<Self>) {
        if self.hosts().next().is_none() {
            return;
        }
        let active = self.active(cx);
        if self.api.active.as_ref() != Some(&active) {
            self.tell("active", active.clone());
            self.api.active = Some(active);
        }
    }

    /// The settings as extensions ask for them: what each declares, with
    /// what the user set, and the few of the editor's own they read.
    fn resolved(&self, cx: &App) -> Value {
        let mut all = self.configuration("", cx);
        if !all.is_object() {
            all = json!({});
        }
        let indent = cx
            .try_global::<Settings>()
            .map_or(4, |settings| settings.indent_size);
        let editor = &mut all["editor"];
        if !editor.is_object() {
            *editor = json!({});
        }
        for (key, value) in [("tabSize", json!(indent)), ("insertSpaces", json!(true))] {
            if editor[key].is_null() {
                editor[key] = value;
            }
        }
        all
    }

    /// The settings changed, or what is installed did.
    pub(crate) fn settings_changed(&mut self, cx: &mut Context<Self>) {
        if self.hosts().next().is_none() {
            return;
        }
        let now = self.resolved(cx);
        if self.api.configuration.as_ref() != Some(&now) {
            self.tell("configuration", json!({ "configuration": now }));
            self.api.configuration = Some(now);
        }
    }

    /// Everything a host is told before the extension is loaded in it.
    pub(crate) fn snapshot(&mut self, cx: &mut Context<Self>) -> Value {
        self.folders_changed();
        let configuration = self.resolved(cx);
        let active = self.active(cx);
        let documents: Vec<Value> = self
            .documents
            .iter()
            .filter_map(|document| document.upgrade())
            .filter_map(|document| {
                let document = document.read(cx);
                Some(opened(document, document.path()?))
            })
            .collect();
        let dark = cx.theme().bg.l < 0.5;
        self.api.configuration = Some(configuration.clone());
        self.api.active = Some(active.clone());
        json!({
            "folders": self.api.folders,
            "configuration": configuration,
            "dark": dark,
            "documents": documents,
            "active": active,
        })
    }

    /// Follows a document from now on: its changes are said to the hosts,
    /// and that it is gone.
    pub(crate) fn follow(&mut self, document: &Entity<Document>, cx: &mut Context<Self>) {
        let id = document.entity_id();
        if self.api.followed.contains_key(&id) {
            return;
        }
        let path = document.read(cx).path().map(Path::to_path_buf);
        let events = cx.subscribe(document, |this, document, event, cx| {
            this.document_event(&document, event, cx)
        });
        let gone = cx.observe_release(document, move |this, _, _| {
            if let Some(Followed {
                path: Some(path), ..
            }) = this.api.followed.remove(&id)
            {
                this.tell("closed", json!({ "uri": uri(&path) }));
            }
        });
        if let Some(path) = &path {
            self.tell("opened", opened(document.read(cx), path));
        }
        self.api.followed.insert(
            id,
            Followed {
                path,
                _events: events,
                _gone: gone,
            },
        );
    }

    fn document_event(
        &mut self,
        document: &Entity<Document>,
        event: &DocumentEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(followed) = self.api.followed.get(&document.entity_id()) else {
            return;
        };
        match event {
            DocumentEvent::Edited { edits, .. } => {
                let (Some(path), true) = (&followed.path, self.hosts().next().is_some()) else {
                    return;
                };
                let document = document.read(cx);
                // Each in the text as it was just before it, in UTF-16
                // columns, which is how an extension counts.
                let changes: Vec<Value> = edits
                    .iter()
                    .map(|edit| {
                        json!({
                            "range": {
                                "start": { "line": edit.start_utf16.row, "character": edit.start_utf16.column },
                                "end": { "line": edit.old_end_utf16.row, "character": edit.old_end_utf16.column },
                            },
                            "text": &*edit.new_text,
                        })
                    })
                    .collect();
                self.tell(
                    "changed",
                    json!({
                        "uri": uri(path),
                        "version": document.version(),
                        "dirty": document.is_dirty(),
                        "changes": changes,
                    }),
                );
            }
            DocumentEvent::Saved => {
                if let Some(path) = &followed.path {
                    self.tell("saved", json!({ "uri": uri(path) }));
                }
            }
            DocumentEvent::PathChanged => {
                let now = document.read(cx).path().map(Path::to_path_buf);
                if now == followed.path {
                    return;
                }
                // To an extension a file under another name is another
                // file: the old one closed, the new one opened.
                if let Some(old) = &followed.path {
                    self.tell("closed", json!({ "uri": uri(old) }));
                }
                if let Some(new) = &now {
                    self.tell("opened", opened(document.read(cx), new));
                }
                if let Some(followed) = self.api.followed.get_mut(&document.entity_id()) {
                    followed.path = now;
                }
                // Its language may be one an extension waits for.
                self.wake(cx);
            }
            _ => {}
        }
    }

    /// What extensions have in the status bar: their items, the work they
    /// say is in progress, and the last message that asked nothing.
    pub fn bar(&self) -> Vec<BarText> {
        let mut items: Vec<&StatusItem> = self
            .api
            .status
            .values()
            .filter(|item| item.visible)
            .collect();
        items.sort_by(|a, b| {
            b.priority
                .partial_cmp(&a.priority)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut bar: Vec<BarText> = items
            .into_iter()
            .map(|item| BarText {
                text: without_icons(&item.text),
                tone: Tone::Plain,
                command: item.command.clone(),
            })
            .collect();
        bar.extend(self.api.progress.values().map(|text| BarText {
            text: format!("{text}..."),
            tone: Tone::Plain,
            command: None,
        }));
        if let Some((tone, text)) = &self.api.note {
            bar.push(BarText {
                text: text.clone(),
                tone: *tone,
                command: None,
            });
        }
        bar.retain(|item| !item.text.is_empty());
        bar
    }

    /// The output channels of an extension, by name.
    pub fn outputs(&self, extension: &str) -> Vec<String> {
        self.api
            .output
            .keys()
            .filter(|(of, _)| of == extension)
            .map(|(_, channel)| channel.clone())
            .collect()
    }

    /// What an extension wrote to a channel so far.
    pub fn output(&self, extension: &str, channel: &str) -> Option<&str> {
        let key = (extension.to_string(), channel.to_string());
        self.api.output.get(&key).map(String::as_str)
    }

    /// Opens what an extension wrote to a channel, in the window in front.
    pub fn show_output(&mut self, extension: &str, channel: &str, cx: &mut Context<Self>) {
        if let Some(text) = self.output(extension, channel).map(str::to_string) {
            let title = format!("Output: {channel}");
            self.ask(Ask::Output { title, text }, cx);
        }
    }

    /// A message that asks nothing: in the status bar for a while, and in
    /// what the extension said.
    fn note(&mut self, id: &str, tone: Tone, text: String, cx: &mut Context<Self>) {
        if let Some(code) = self.code.get_mut(id) {
            let level = match tone {
                Tone::Plain => "info",
                Tone::Warning => "warn",
                Tone::Error => "error",
            };
            code.said.push_back((level.into(), text.clone()));
        }
        let line = text.lines().next().unwrap_or_default().to_string();
        self.api.note = Some((tone, line));
        self.api.notes += 1;
        let shown = self.api.notes;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTE).await;
            this.update(cx, |this, cx| {
                if this.api.notes == shown {
                    this.api.note = None;
                    cx.emit(ExtensionEvent::Bar);
                }
            })
            .ok();
        })
        .detach();
        cx.emit(ExtensionEvent::Bar);
    }

    /// Says in the status bar that something of an extension went wrong.
    pub fn report(&mut self, text: String, cx: &mut Context<Self>) {
        self.note("", Tone::Error, text, cx);
    }

    /// Everything of an extension that was on screen goes with its code.
    pub(crate) fn clear_shown(&mut self, id: &str, cx: &mut Context<Self>) {
        self.api.webviews.retain(|(owner, _), _| owner != id);
        self.api.web_posts.retain(|(owner, _), _| owner != id);
        cx.emit(ExtensionEvent::Webviews);
        self.clear_extension_decorations(id, cx);
        self.api.views.retain(|(owner, _), _| owner != id);
        cx.emit(ExtensionEvent::Views);
        let (status, progress) = (self.api.status.len(), self.api.progress.len());
        self.api.status.retain(|(of, _), _| of != id);
        self.api.progress.retain(|(of, _), _| of != id);
        if (status, progress) != (self.api.status.len(), self.api.progress.len()) {
            cx.emit(ExtensionEvent::Bar);
        }
    }

    /// What the conditions of extensions are read against that is the
    /// same wherever they are read: the machine, and what extensions said
    /// of themselves. The window adds the file in front.
    pub fn facts(&self) -> std::collections::HashMap<String, Value> {
        let mut facts = self.api.contexts.clone();
        for (name, os) in [
            ("isMac", "macos"),
            ("isLinux", "linux"),
            ("isWindows", "windows"),
        ] {
            facts.insert(name.into(), json!(std::env::consts::OS == os));
        }
        facts
    }

    /// What the key an extension bound to `command` runs, if it bound one.
    fn keyed(extension: &extension::Extension, command: &str) -> Option<RunExtensionCommand> {
        let key = extension.keys.iter().find(|key| key.command == command)?;
        Some(RunExtensionCommand {
            command: command.to_string(),
            args: key.args.clone(),
            when: key.when.clone(),
        })
    }

    /// The commands the palette lists: every one an extension whose code
    /// may run names in its manifest, but for the ones it keeps out of the
    /// palette or that cannot be run as things are. Choosing one starts
    /// the extension if it waits for it.
    pub fn palette(&self, facts: &std::collections::HashMap<String, Value>) -> Vec<Offered> {
        let fact = |name: &str| facts.get(name).cloned();
        let holds = |when: &Option<String>| {
            when.as_deref()
                .is_none_or(|when| extension::when::holds(when, &fact))
        };
        let mut offered = Vec::new();
        for extension in self.installed.iter().filter(|e| self.may_run(e)) {
            for command in &extension.contributed {
                let kept_out = extension.menus.iter().any(|menu| {
                    menu.menu == "commandPalette"
                        && menu.command == command.command
                        && !holds(&menu.when)
                });
                if !kept_out && holds(&command.enablement) {
                    offered.push(Offered {
                        title: command.title.clone(),
                        // Asked for by name, it is given nothing, and
                        // runs whatever its key would wait for.
                        action: RunExtensionCommand {
                            command: command.command.clone(),
                            args: Value::Null,
                            when: None,
                        },
                        keyed: Self::keyed(extension, &command.command),
                    });
                }
            }
        }
        offered
    }

    /// The commands extensions put in `menu` (`editor/context`,
    /// `explorer/context`) that are there as things are. Each is given
    /// `target`, the file the menu is for, as VS Code gives it.
    pub fn menu(
        &self,
        menu: &str,
        facts: &std::collections::HashMap<String, Value>,
        target: Option<&Path>,
    ) -> Vec<Offered> {
        let fact = |name: &str| facts.get(name).cloned();
        let holds = |when: &Option<String>| {
            when.as_deref()
                .is_none_or(|when| extension::when::holds(when, &fact))
        };
        let args = match target {
            Some(path) => json!([{ "$uri": uri(path) }]),
            None => Value::Null,
        };
        let mut offered: Vec<Offered> = Vec::new();
        for extension in self.installed.iter().filter(|e| self.may_run(e)) {
            for item in extension.menus.iter().filter(|item| item.menu == menu) {
                let named = extension
                    .contributed
                    .iter()
                    .find(|command| command.command == item.command);
                let enabled = named.is_none_or(|command| holds(&command.enablement));
                if !holds(&item.when) || !enabled {
                    continue;
                }
                let title = named.map_or(item.command.clone(), |command| command.title.clone());
                if offered
                    .iter()
                    .all(|known| known.action.command != item.command)
                {
                    offered.push(Offered {
                        title,
                        action: RunExtensionCommand {
                            command: item.command.clone(),
                            args: args.clone(),
                            when: None,
                        },
                        keyed: Self::keyed(extension, &item.command),
                    });
                }
            }
            // Its editors for a file of this name, each a way to open it.
            for editor in &extension.custom_editors {
                let Some(path) = target.filter(|path| editor.opens(path)) else {
                    continue;
                };
                offered.push(Offered {
                    title: format!("Open with {}", editor.name),
                    action: RunExtensionCommand {
                        command: OPEN_WITH.into(),
                        args: json!([extension.id, editor.view_type, { "$uri": uri(path) }]),
                        when: None,
                    },
                    keyed: None,
                });
            }
        }
        offered
    }

    /// Gives the keymap the keys of the extensions whose code may run.
    /// Nothing is bound again while they are the same.
    pub(crate) fn sync_keys(&mut self, cx: &mut Context<Self>) {
        let mut sections = Vec::new();
        for extension in self.installed.iter().filter(|e| self.may_run(e)) {
            for key in &extension.keys {
                let action = json!({ "command": key.command, "args": key.args, "when": key.when });
                sections.push(json!({
                    "context": key_context(key.when.as_deref()),
                    "bindings": { key.keys.as_str(): ["workspace::RunExtensionCommand", action] },
                }));
            }
        }
        let source = match sections.is_empty() {
            true => String::new(),
            false => Value::Array(sections).to_string(),
        };
        if source != self.api.keys {
            for error in crate::settings::set_extension_keys(&source, cx) {
                eprintln!("{error}");
            }
            self.api.keys = source;
        }
    }

    /// The terminal an extension made ended: what ran in it did, or its
    /// tab was closed. `code` is what the program ended with, if it ended.
    pub fn terminal_closed(&self, extension: &str, terminal: u64, code: Option<i32>) {
        if let Some(host) = self.code.get(extension).and_then(|code| code.host.as_ref()) {
            host.notify("terminal.closed", json!({ "id": terminal, "code": code }));
        }
    }

    /// The tasks of extensions: of every one whose code runs, and of the
    /// ones that wait for their kind of task to be asked for, which are
    /// started for this.
    pub fn tasks(&mut self, cx: &mut Context<Self>) -> Task<Vec<OfferedTask>> {
        let asking: Vec<String> = self
            .installed
            .iter()
            .filter(|extension| self.may_run(extension))
            .filter(|extension| {
                let waits = |(_, wakes): (_, &[String])| {
                    wakes.iter().any(|event| event.starts_with("onTaskType:"))
                };
                self.code.contains_key(&extension.id) || extension.node().is_some_and(waits)
            })
            .map(|extension| extension.id.clone())
            .collect();
        let asked: Vec<_> = asking
            .into_iter()
            .map(|id| {
                let listing = self.ask_host(&id, "tasks.fetch", json!({}), cx);
                (id, listing)
            })
            .collect();
        cx.background_executor().spawn(async move {
            let mut tasks = Vec::new();
            for (extension, listing) in asked {
                let Ok(Value::Array(listed)) = listing.await else {
                    continue;
                };
                for (index, task) in listed.iter().enumerate() {
                    let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
                    tasks.push(OfferedTask {
                        extension: extension.clone(),
                        index,
                        name: text(&task["name"]),
                        source: text(&task["source"]),
                    });
                }
            }
            tasks
        })
    }

    /// Runs a task [`Self::tasks`] listed, in a terminal of the dock.
    pub fn run_task(
        &mut self,
        task: &OfferedTask,
        cx: &mut Context<Self>,
    ) -> Task<Result<Value, String>> {
        let params = json!({ "index": task.index });
        self.ask_host(&task.extension, "tasks.run", params, cx)
    }

    /// Says to the extensions whose code runs that a program began to be
    /// debugged, or that it ended.
    pub fn debug_session(&self, began: bool, session: Value) {
        self.tell(
            if began {
                "debug.started"
            } else {
                "debug.ended"
            },
            session,
        );
    }

    /// The extensions whose code answers for files of a language as a
    /// language server would: the ones that registered something for one
    /// of the names the language goes by, or for every language.
    pub fn language_hosts(&self, names: &[String]) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .code
            .iter()
            .filter(|(_, code)| code.host.is_some())
            .filter(|(_, code)| {
                let for_it = |language: &String| language == "*" || names.contains(language);
                code.languages.iter().any(for_it)
            })
            .map(|(id, _)| id.clone())
            .collect();
        hosts.sort();
        hosts
    }

    /// The language server that the host of `extension` is: what it is
    /// asked goes to the host, a message at a time, and what the host
    /// answers comes back through a link the store keeps. Asked for again,
    /// it is a new server, and the one before it is closed. The number
    /// says which asking this was.
    pub fn language_server(
        &mut self,
        extension: &str,
    ) -> Option<(
        Arc<lsp::LanguageServer>,
        futures::channel::mpsc::UnboundedReceiver<lsp::Notification>,
        usize,
    )> {
        let code = self.code.get_mut(extension)?;
        let host = code.host.clone()?;
        let (server, link, notes) = lsp::LanguageServer::linked(extension, move |message| {
            host.notify("lsp", json!({ "message": message }));
        })
        .ok()?;
        code.link = Some(link);
        code.links += 1;
        Some((server, notes, code.links))
    }

    /// Whether the server given under this number is still the one its
    /// extension speaks through.
    pub fn is_language_server(&self, extension: &str, number: usize) -> bool {
        self.code
            .get(extension)
            .is_some_and(|code| code.link.is_some() && code.links == number)
    }

    /// Which extensions answer for which languages changed. Said once
    /// this update is over: the documents ask this store which are theirs.
    pub(crate) fn languages_changed(&mut self, cx: &mut Context<Self>) {
        cx.defer(|cx| {
            if let Some(lsp) = crate::lsp_store::LspStore::global(cx) {
                lsp.update(cx, |lsp, cx| lsp.hosts_changed(cx));
            }
        });
    }

    /// Something a host said that waits for no answer.
    pub(crate) fn said(&mut self, id: &str, method: &str, params: Value, cx: &mut Context<Self>) {
        let text = |value: &Value| value.as_str().map(str::to_string);
        match method {
            "webview" | "webview.reveal" => self.webview_said(id, method, params, cx),
            "decorations" | "decorations.gone" | "files.provider" | "files.changed" => {
                self.decoration_said(id, method, params, cx)
            }
            "view" | "view.changed" => self.view_said(id, method, params, cx),
            // An answer of the language server its host is, or something
            // that server says on its own.
            "lsp" => {
                if let Some(link) = self.code.get(id).and_then(|code| code.link.as_ref()) {
                    link.receive(params["message"].clone());
                }
            }
            // What it registered in code changed. What it can do is read
            // once by the editor, when a server starts, so the server it
            // was is closed and the documents get a new one.
            "providers" => {
                let mut languages: Vec<String> = params["languages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(text)
                    .collect();
                languages.sort();
                languages.dedup();
                let Some(code) = self.code.get_mut(id) else {
                    return;
                };
                code.languages = languages;
                code.link = None;
                self.languages_changed(cx);
            }
            "status" => {
                let key = (id.to_string(), params["id"].as_u64().unwrap_or_default());
                if params["gone"] == true {
                    self.api.status.remove(&key);
                } else {
                    let command = text(&params["command"])
                        .filter(|command| !command.is_empty())
                        .map(|command| (command, params["args"].clone()));
                    self.api.status.insert(
                        key,
                        StatusItem {
                            text: text(&params["text"]).unwrap_or_default(),
                            tooltip: text(&params["tooltip"]),
                            command,
                            visible: params["visible"] == true,
                            right: params["right"] == true,
                            priority: params["priority"].as_f64().unwrap_or_default(),
                        },
                    );
                }
                cx.emit(ExtensionEvent::Bar);
            }
            "progress" => {
                let key = (id.to_string(), params["id"].as_u64().unwrap_or_default());
                match text(&params["text"]) {
                    Some(text) if params["done"] != true => {
                        self.api.progress.insert(key, text);
                    }
                    _ => {
                        self.api.progress.remove(&key);
                    }
                }
                cx.emit(ExtensionEvent::Bar);
            }
            "output" => {
                let Some(channel) = text(&params["channel"]) else {
                    return;
                };
                let key = (id.to_string(), channel.clone());
                if params["gone"] == true {
                    self.api.output.remove(&key);
                    return;
                }
                let kept = self.api.output.entry(key).or_default();
                if params["clear"] == true {
                    kept.clear();
                }
                if let Some(more) = params["text"].as_str() {
                    kept.push_str(more);
                    // The oldest lines go first, whole.
                    if kept.len() > OUTPUT {
                        let mut cut = kept.len() - OUTPUT;
                        while !kept.is_char_boundary(cut) {
                            cut += 1;
                        }
                        let cut = kept[cut..].find('\n').map_or(cut, |line| cut + line + 1);
                        kept.drain(..cut);
                    }
                }
                if params["show"] == true {
                    self.show_output(id, &channel, cx);
                }
            }
            // A terminal of its own: made, typed into, shown, closed.
            "terminal.create" | "terminal.send" | "terminal.show" | "terminal.dispose" => {
                let what = match method {
                    "terminal.create" => TerminalAsk::Create {
                        name: text(&params["name"]),
                        program: text(&params["program"]),
                        args: params["args"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(text)
                            .collect(),
                        cwd: text(&params["cwd"]).map(PathBuf::from),
                        env: params["env"]
                            .as_object()
                            .into_iter()
                            .flatten()
                            .filter_map(|(name, value)| Some((name.clone(), text(value)?)))
                            .collect(),
                        keep: params["keep"] == true,
                        show: params["show"] == true,
                    },
                    "terminal.send" => TerminalAsk::Send(text(&params["text"]).unwrap_or_default()),
                    "terminal.show" => TerminalAsk::Show,
                    _ => TerminalAsk::Dispose,
                };
                let terminal = params["id"].as_u64().unwrap_or_default();
                self.ask(
                    Ask::Terminal {
                        extension: id.to_string(),
                        terminal,
                        what,
                    },
                    cx,
                );
            }
            // The extension moved the selection or asks to see a place.
            "select" | "reveal" => {
                let range = match method {
                    "reveal" => serde_json::from_value(params["range"].clone()).ok(),
                    _ => serde_json::from_value(json!({
                        "start": params["selections"][0]["anchor"],
                        "end": params["selections"][0]["active"],
                    }))
                    .ok(),
                };
                if let (Some(path), Some(at)) = (path_of(&params["uri"]), range) {
                    let at = Some(at);
                    self.ask(
                        Ask::Show {
                            path,
                            at,
                            reply: None,
                        },
                        cx,
                    );
                }
            }
            _ => {}
        }
    }

    /// Something a host asks and waits for.
    pub(crate) fn asked(
        &mut self,
        id: &str,
        host: Arc<VsHost>,
        asked: Value,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
    ) {
        let reply = Reply {
            host,
            id: Some(asked),
        };
        let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
        match method {
            "webview.post" => {
                let key = (id.to_string(), text(&params["id"]));
                if self.api.webviews.contains_key(&key) {
                    let posts = self.api.web_posts.entry(key).or_default();
                    if posts.len() < 256 {
                        posts.push_back((params["message"].clone(), reply));
                        cx.emit(ExtensionEvent::Webviews);
                    } else {
                        reply.send(Ok(false.into()));
                    }
                } else {
                    reply.send(Ok(false.into()));
                }
            }
            "executeCommand" => {
                let command = text(&params["id"]);
                let answer = self.command_from(Some(id), &command, params["args"].clone(), cx);
                cx.background_executor()
                    .spawn(async move { reply.send(answer.await) })
                    .detach();
            }
            "message" => {
                let items: Vec<String> = params["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(text)
                    .collect();
                let said = text(&params["text"]);
                if items.is_empty() && params["modal"] != true {
                    let tone = match params["level"].as_str() {
                        Some("error") => Tone::Error,
                        Some("warning") => Tone::Warning,
                        _ => Tone::Plain,
                    };
                    self.note(id, tone, said, cx);
                    reply.send(Ok(Value::Null));
                    return;
                }
                // Read and closed, or one of its answers chosen.
                let rows = match items.is_empty() {
                    true => vec!["OK".to_string()],
                    false => items,
                };
                let rows = rows
                    .into_iter()
                    .map(|label| PickRow {
                        label,
                        ..Default::default()
                    })
                    .collect();
                let title = match params["detail"].as_str() {
                    Some(detail) if !detail.is_empty() => format!("{said} {detail}"),
                    _ => said,
                };
                self.ask(Ask::Pick { title, rows, reply }, cx);
            }
            "pick" => {
                let rows = params["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|item| PickRow {
                        label: text(&item["label"]),
                        description: text(&item["description"]),
                        detail: text(&item["detail"]),
                    })
                    .collect();
                let title = [&params["placeholder"], &params["title"]]
                    .into_iter()
                    .map(text)
                    .find(|title| !title.is_empty())
                    .unwrap_or_else(|| "Pick one".into());
                self.ask(Ask::Pick { title, rows, reply }, cx);
            }
            "input" => {
                let title = [&params["prompt"], &params["placeholder"], &params["title"]]
                    .into_iter()
                    .map(text)
                    .find(|title| !title.is_empty())
                    .unwrap_or_else(|| "Type a value".into());
                let value = text(&params["value"]);
                self.ask(
                    Ask::Input {
                        title,
                        value,
                        reply,
                    },
                    cx,
                );
            }
            "applyEdit" => match serde_json::from_value(json!({ "changes": params["changes"] })) {
                Ok(edit) => self.ask(Ask::Edit { edit, reply }, cx),
                Err(error) => reply.send(Err(error.to_string())),
            },
            "show" => match path_of(&params["uri"]) {
                Some(path) => {
                    let at = serde_json::from_value(json!({
                        "start": params["selection"]["anchor"],
                        "end": params["selection"]["active"],
                    }))
                    .ok();
                    let reply = Some(reply);
                    self.ask(Ask::Show { path, at, reply }, cx);
                }
                None => reply.send(Err("Only files can be shown".into())),
            },
            "save" => {
                let path = path_of(&params["uri"]);
                match path {
                    Some(_) => self.ask(Ask::Save { path, reply }, cx),
                    None => reply.send(Err("Only files can be saved".into())),
                }
            }
            "saveAll" => self.ask(Ask::Save { path: None, reply }, cx),
            "updateConfiguration" => {
                let written = self.set_setting(text(&params["key"]), params["value"].clone(), cx);
                cx.background_executor()
                    .spawn(async move { reply.send(written.await.map(|()| Value::Null)) })
                    .detach();
            }
            // A launch the extension put together itself, for a debugger
            // some installed extension has. Started once this update is
            // over: the debugger asks this store for the adapter.
            "debug.start" => {
                let configuration = params["configuration"].clone();
                let kind = text(&configuration["type"]);
                let owner = self
                    .installed
                    .iter()
                    .filter(|e| e.origin == extension::Origin::VsCode)
                    .filter(|e| !self.is_off(e.origin, &e.id))
                    .find(|e| e.debuggers.iter().any(|debugger| debugger.name == kind))
                    .map(|e| e.id.clone());
                let root = path_of(&params["folder"])
                    .or_else(|| params["folder"].as_str().map(PathBuf::from))
                    .or_else(|| self.api.folders.first().cloned());
                let (Some(extension), Some(root)) = (owner, root) else {
                    reply.send(Ok(json!(false)));
                    return;
                };
                let name = match text(&configuration["name"]) {
                    name if name.is_empty() => kind.clone(),
                    name => name,
                };
                let launch = extension::host::DebugLaunch {
                    label: name.clone(),
                    adapter: kind,
                    program: text(&configuration["program"]),
                    cwd: Some(root.display().to_string()),
                    ..Default::default()
                };
                let config = crate::debug_launch::LaunchConfig {
                    name,
                    adapter: Some(crate::debug_launch::ExtensionAdapter {
                        extension,
                        launch,
                        configuration: Some(configuration),
                    }),
                    ..Default::default()
                };
                cx.defer(move |cx| {
                    let debug = crate::debug::DebugStore::global(cx);
                    debug.update(cx, |debug, cx| debug.start(config, root, cx));
                });
                reply.send(Ok(json!(true)));
            }
            "debug.stop" => {
                cx.defer(|cx| {
                    if let Some(debug) = crate::debug::DebugStore::try_global(cx) {
                        debug.update(cx, |debug, cx| debug.stop(cx));
                    }
                });
                reply.send(Ok(Value::Null));
            }
            "clipboardRead" => {
                let text = cx.read_from_clipboard().and_then(|item| item.text());
                reply.send(Ok(json!(text.unwrap_or_default())));
            }
            "clipboardWrite" => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text(&params["text"])));
                reply.send(Ok(Value::Null));
            }
            "openExternal" => {
                let url = text(&params["url"]);
                let web = ["http://", "https://", "mailto:"]
                    .iter()
                    .any(|scheme| url.starts_with(scheme));
                if web {
                    cx.open_url(&url);
                }
                reply.send(Ok(json!(web)));
            }
            _ => reply.send(Err(format!("Solder does not do {method}"))),
        }
    }

    /// Sets `key` in `settings.json`, as an extension's `update` does, and
    /// reads the file again.
    fn set_setting(
        &mut self,
        key: String,
        value: Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        if key.trim().is_empty() {
            return Task::ready(Err("A setting has a name".into()));
        }
        let config = self.config.clone();
        cx.spawn(async move |this, cx| {
            let dir = config.clone();
            cx.background_executor()
                .spawn(async move {
                    let file = dir.join("settings.json");
                    let source = std::fs::read_to_string(&file).unwrap_or_default();
                    let source = if source.trim().is_empty() {
                        "{}\n".to_string()
                    } else {
                        source
                    };
                    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                    std::fs::write(&file, import::jsonc::set_key(&source, &key, &value))
                        .map_err(|e| e.to_string())
                })
                .await?;
            // At once, not when the watcher notices.
            this.update(cx, |_, cx| crate::settings::reload_from(&config, cx))
                .map_err(|_| "The editor is closing".to_string())
        })
    }

    /// Runs a command the code of a VS Code extension has, with what it is
    /// given, and gives what it answers. An extension that waits for the
    /// command is started for it.
    pub fn run_command(
        &mut self,
        command: &str,
        args: Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<Value, String>> {
        self.command_from(None, command, args, cx)
    }
}
