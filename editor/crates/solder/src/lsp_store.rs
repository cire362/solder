//! Language servers for every open document: starts them on demand, keeps
//! them in sync with edits and turns their diagnostics into squiggles.

use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::{FutureExt, StreamExt, future::BoxFuture};
use gpui::{
    App, AppContext, Context, Entity, EntityId, Global, SharedString, Subscription, Task,
    WeakEntity,
};
use lsp::{Encoding, LanguageServer, Notification, ServerCommand, path_to_uri, types as lt};
use text::{Buffer, Point};

use crate::{
    document::{Diagnostic, Document, DocumentEvent, Severity},
    extension_store::ExtensionServer,
    settings::Settings,
};

/// A language server Solder knows how to start.
#[derive(Clone, Copy, Debug)]
pub struct ServerSpec {
    pub name: &'static str,
    program: &'static str,
    args: &'static [&'static str],
    /// Files that mark the project root for this server.
    root_markers: &'static [&'static str],
    install_hint: &'static str,
}

pub fn spec_for(language: &str) -> Option<ServerSpec> {
    Some(match language {
        "Rust" => ServerSpec {
            name: "rust-analyzer",
            program: "rust-analyzer",
            args: &[],
            root_markers: &["Cargo.toml"],
            install_hint: "rustup component add rust-analyzer",
        },
        "TypeScript" | "TSX" | "JavaScript" => ServerSpec {
            name: "typescript-language-server",
            program: "typescript-language-server",
            args: &["--stdio"],
            root_markers: &["tsconfig.json", "jsconfig.json", "package.json"],
            install_hint: "npm i -g typescript typescript-language-server",
        },
        "Go" => ServerSpec {
            name: "gopls",
            program: "gopls",
            args: &[],
            root_markers: &["go.mod"],
            install_hint: "go install golang.org/x/tools/gopls@latest",
        },
        "Python" => ServerSpec {
            name: "pyright",
            program: "pyright-langserver",
            args: &["--stdio"],
            root_markers: &["pyproject.toml", "setup.py", "requirements.txt"],
            install_hint: "npm i -g pyright",
        },
        "CSS" => ServerSpec {
            name: "css-language-server",
            program: "vscode-css-language-server",
            args: &["--stdio"],
            root_markers: &["package.json"],
            install_hint: "npm i -g vscode-langservers-extracted",
        },
        // The servers Zed has built in for the languages it has built in,
        // found on the PATH like the ones above. Markdown and shell have
        // none there either: theirs come with extensions (Marksman, Basher),
        // which name these languages as Zed does.
        "C" | "C++" => ServerSpec {
            name: "clangd",
            program: "clangd",
            args: &[],
            root_markers: &["compile_commands.json", "compile_flags.txt", ".clangd"],
            install_hint: if cfg!(target_os = "macos") {
                "xcode-select --install"
            } else {
                "apt install clangd"
            },
        },
        "YAML" => ServerSpec {
            name: "yaml-language-server",
            program: "yaml-language-server",
            args: &["--stdio"],
            root_markers: &["package.json"],
            install_hint: "npm i -g yaml-language-server",
        },
        "JSON" => ServerSpec {
            name: "json-language-server",
            program: "vscode-json-language-server",
            args: &["--stdio"],
            root_markers: &["package.json"],
            install_hint: "npm i -g vscode-langservers-extracted",
        },
        _ => return None,
    })
}

/// Nearest ancestor of `file` holding one of `markers`, else its folder.
fn find_root(file: &Path, markers: &[&str]) -> PathBuf {
    let dir = file.parent().unwrap_or(file);
    dir.ancestors()
        .find(|d| markers.iter().any(|m| d.join(m).exists()))
        .unwrap_or(dir)
        .to_path_buf()
}

/// Project-local `node_modules/.bin` first, then `PATH`, then the usual
/// install locations (apps started from the Dock get a minimal `PATH`).
pub(crate) fn find_program(program: &str, root: &Path) -> Option<PathBuf> {
    for dir in root.ancestors() {
        let local = dir.join("node_modules/.bin").join(program);
        if local.is_file() {
            return Some(local);
        }
    }
    let home = dirs::home_dir().unwrap_or_default();
    let extra = [
        home.join(".cargo/bin"),
        home.join(".local/bin"),
        home.join("go/bin"),
        home.join("Library/pnpm"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .chain(extra)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

/// Server names live as long as the program: they key the running servers.
/// The ones extensions bring are few, so each is kept once and never freed.
fn intern(name: &str) -> &'static str {
    static NAMES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(known) = names.iter().find(|n| **n == name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.push(leaked);
    leaked
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ServerKey {
    name: &'static str,
    root: PathBuf,
}

enum ServerState {
    Starting,
    Running {
        server: Arc<LanguageServer>,
        _notifications: Task<()>,
    },
    Failed,
}

struct DocEntry {
    document: WeakEntity<Document>,
    /// The servers the document belongs to: the one Solder knows for its
    /// language, then every one an extension brings for it.
    servers: Vec<Attached>,
    uri: lt::Uri,
    version: i32,
    _subscriptions: [Subscription; 2],
}

/// A document's place in one server.
struct Attached {
    key: ServerKey,
    /// The server runs and was told the document is open.
    opened: bool,
    /// What the server calls this document's language, when an extension
    /// says; otherwise the document's own id is used.
    language_id: Option<String>,
}

/// A code action with the server it came from: its edits are in that
/// server's encoding, and resolving or running it goes back there.
#[derive(Clone, Debug)]
pub struct ServerAction {
    pub action: lt::CodeActionOrCommand,
    pub encoding: Encoding,
    pub server: &'static str,
}

/// A request on its way to one server: the server's name, its encoding and
/// the answer to wait for.
pub type Asked<R> = (
    &'static str,
    Encoding,
    BoxFuture<'static, lsp::Result<<R as lt::request::Request>::Result>>,
);

/// Whether a server said it answers `method`. One that did not say either
/// way is asked.
fn supports(caps: &lt::ServerCapabilities, method: &str) -> bool {
    fn yes<T>(provider: &Option<lt::OneOf<bool, T>>) -> bool {
        !matches!(provider, None | Some(lt::OneOf::Left(false)))
    }
    match method {
        "textDocument/completion" => caps.completion_provider.is_some(),
        "textDocument/hover" => !matches!(
            caps.hover_provider,
            None | Some(lt::HoverProviderCapability::Simple(false))
        ),
        "textDocument/signatureHelp" => caps.signature_help_provider.is_some(),
        "textDocument/definition" => yes(&caps.definition_provider),
        "textDocument/references" => yes(&caps.references_provider),
        "textDocument/documentSymbol" => yes(&caps.document_symbol_provider),
        "workspace/symbol" => yes(&caps.workspace_symbol_provider),
        "textDocument/rename" => yes(&caps.rename_provider),
        "textDocument/formatting" => yes(&caps.document_formatting_provider),
        "textDocument/codeAction" => !matches!(
            caps.code_action_provider,
            None | Some(lt::CodeActionProviderCapability::Simple(false))
        ),
        _ => true,
    }
}

/// What extensions added to one of the servers Solder knows: Vue's adds its
/// plugin to the TypeScript server's options.
#[derive(Default)]
struct SetUp {
    /// The extension servers that were asked already, by id.
    asked: Vec<String>,
    options: Option<serde_json::Value>,
    settings: Option<serde_json::Value>,
}

/// A server Solder knows that also serves a language an extension brings,
/// once that extension is installed, and what it is told the language is.
/// A Vue file is the TypeScript server's too: Vue's own server leaves the
/// script to it, and asks it questions through the editor.
fn companion(language: &str) -> Option<(ServerSpec, &'static str)> {
    match language {
        "Vue.js" => Some((spec_for("TypeScript")?, "vue.js")),
        _ => None,
    }
}

/// What Vue's server sends to have the TypeScript server asked something,
/// and what it is sent back.
enum TsserverResponse {}

impl lt::notification::Notification for TsserverResponse {
    type Params = serde_json::Value;
    const METHOD: &'static str = "tsserver/response";
}

impl Attached {
    fn new(key: ServerKey, language_id: Option<String>) -> Self {
        Self {
            key,
            opened: false,
            language_id,
        }
    }
}

#[derive(Default)]
pub struct LspStore {
    servers: HashMap<ServerKey, ServerState>,
    docs: HashMap<EntityId, DocEntry>,
    /// What extensions added to the servers Solder knows.
    set_up: HashMap<ServerKey, SetUp>,
    /// How many times each server was started. A start that finishes after
    /// a later one began is dropped.
    starts: HashMap<ServerKey, u64>,
    /// Latest progress or error message, shown in the status bar.
    status: Option<SharedString>,
    /// Which servers the settings chose when the documents were last
    /// given theirs.
    chosen: Choice,
}

/// What the settings say of which servers start: the choice of each
/// language, and the servers turned off by name.
type Choice = (
    std::collections::BTreeMap<String, crate::settings::LanguageSettings>,
    Vec<String>,
);

fn choice(settings: &Settings) -> Choice {
    let off = settings
        .language_servers
        .iter()
        .filter(|(_, server)| server.disabled)
        .map(|(name, _)| name.clone())
        .collect();
    (settings.languages.clone(), off)
}

/// A server asked the client to apply an edit (usually after a code action
/// command). Workspaces apply the parts under their root.
pub enum LspStoreEvent {
    ApplyEdit {
        edit: lt::WorkspaceEdit,
        encoding: Encoding,
    },
}

impl gpui::EventEmitter<LspStoreEvent> for LspStore {}

struct GlobalLspStore(Entity<LspStore>);

impl Global for GlobalLspStore {}

pub fn init(cx: &mut App) {
    let store = cx.new(|_| LspStore::default());
    cx.on_app_quit({
        let store = store.clone();
        move |cx| {
            let servers = store.update(cx, |s, _| s.shutdown_all());
            async move {
                // Language servers are child processes; without this they
                // would outlive the editor.
                for server in servers {
                    server.kill();
                }
            }
        }
    })
    .detach();
    // The settings chose other servers: open files go to them at once,
    // not when they are next opened.
    cx.observe_global::<Settings>({
        let store = store.downgrade();
        move |cx| {
            store
                .update(cx, |store, cx| {
                    let now = choice(Settings::get(cx));
                    if store.chosen != now {
                        store.chosen = now;
                        store.extension_servers_changed(cx);
                    }
                })
                .ok();
        }
    })
    .detach();
    cx.set_global(GlobalLspStore(store));
}

/// The servers of a language that start, in order: `available` are the
/// ones there are, and `choice` is which of them, as Zed writes it. A name
/// starts that server, `!name` keeps it from starting, and `...` stands for
/// all the others in the order they come. Without `...`, a server that is
/// not named does not start.
pub fn chosen_servers<'a>(available: &[&'a str], choice: &[&str]) -> Vec<&'a str> {
    let named = |name: &str| {
        choice
            .iter()
            .any(|entry| entry.strip_prefix('!').unwrap_or(entry) == name)
    };
    let mut chosen: Vec<&'a str> = Vec::new();
    for entry in choice {
        if *entry == "..." {
            for name in available {
                if !named(name) && !chosen.contains(name) {
                    chosen.push(name);
                }
            }
        } else if !entry.starts_with('!')
            && let Some(name) = available.iter().find(|name| *name == entry)
            && !chosen.contains(name)
        {
            chosen.push(name);
        }
    }
    chosen
}

/// Which servers start for a language whose extension brings several
/// that do the same work, where the user has not said. These are Zed's
/// own choices (its default settings, read on 2026-10-10), so that an
/// extension made for Zed starts here what it starts there. Languages
/// Solder has a server of its own for are not here: that one starts.
fn default_servers(language: &str) -> Option<&'static [&'static str]> {
    const ELIXIR: &[&str] = &[
        "elixir-ls",
        "!expert",
        "!dexter",
        "!next-ls",
        "!lexical",
        "...",
    ];
    Some(match language {
        "CSharp" => &["roslyn", "!csharp-ls", "!omnisharp", "..."],
        "Elixir" => &[
            "elixir-ls",
            "!expert",
            "!dexter",
            "!next-ls",
            "!lexical",
            "!emmet-language-server",
            "...",
        ],
        "EEx" | "HEEx" => ELIXIR,
        "Erlang" => &["erlang-ls", "!elp", "..."],
        "HTML+ERB" => &["herb", "!ruby-lsp", "..."],
        "JS+ERB" | "YAML+ERB" => &["!ruby-lsp", "..."],
        "Kotlin" => &["!kotlin-language-server", "kotlin-lsp", "..."],
        "PHP" => &["phpactor", "!intelephense", "!phptools", "!phpantom", "..."],
        "Proto" => &["buf", "!protols", "!protobuf-language-server", "..."],
        "Ruby" => &[
            "solargraph",
            "!ruby-lsp",
            "!rubocop",
            "!sorbet",
            "!steep",
            "!kanayago",
            "!fuzzy-ruby-server",
            "...",
        ],
        "Starlark" => &["starpls", "!buck2-lsp", "!tilt", "..."],
        "SystemVerilog" => &["slang", "!verible", "!veridian", "!svls", "..."],
        _ => return None,
    })
}

impl LspStore {
    pub fn global(cx: &App) -> Option<Entity<LspStore>> {
        cx.try_global::<GlobalLspStore>().map(|g| g.0.clone())
    }

    /// Starts tracking a document; opens it on its server once that is up.
    pub fn register(document: &Entity<Document>, cx: &mut App) {
        if let Some(store) = Self::global(cx) {
            store.update(cx, |s, cx| s.register_document(document, cx));
        }
    }

    /// Shows `text` where the servers' progress goes; `None` clears it.
    pub fn set_status(&mut self, text: Option<SharedString>, cx: &mut Context<Self>) {
        self.status = text;
        cx.notify();
    }

    pub fn status(&self) -> Option<&SharedString> {
        self.status.as_ref()
    }

    /// The servers a document belongs to, the first being the one asked
    /// what only one can answer.
    #[cfg(test)]
    pub fn servers_of(&self, document: EntityId) -> Vec<&'static str> {
        self.docs
            .get(&document)
            .map(|entry| entry.servers.iter().map(|s| s.key.name).collect())
            .unwrap_or_default()
    }

    fn register_document(&mut self, document: &Entity<Document>, cx: &mut Context<Self>) {
        let id = document.entity_id();
        if self.docs.contains_key(&id) {
            return;
        }
        let subscriptions = [
            cx.subscribe(document, |this, doc, event, cx| match event {
                DocumentEvent::Edited { edits, .. } => this.did_change(doc.entity_id(), edits, cx),
                DocumentEvent::Saved => this.did_save(doc.entity_id()),
                DocumentEvent::PathChanged => {
                    // Close under the old name, open under the new one.
                    this.close(doc.entity_id());
                    this.attach(doc.entity_id(), cx);
                }
                DocumentEvent::DirtyChanged
                | DocumentEvent::DiagnosticsChanged
                | DocumentEvent::GitChanged => {}
            }),
            cx.observe_release(document, move |this, _, _| {
                this.close(id);
                this.docs.remove(&id);
            }),
        ];
        self.docs.insert(
            id,
            DocEntry {
                document: document.downgrade(),
                uri: path_to_uri(Path::new("/")),
                version: 0,
                servers: Vec::new(),
                _subscriptions: subscriptions,
            },
        );
        self.attach(id, cx);
    }

    /// Works out which servers a document belongs to and gets it opened in
    /// each: the one Solder knows for its language, and every one an
    /// installed extension brings for it.
    fn attach(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(entry) = self.docs.get_mut(&id) else {
            return;
        };
        let Some(document) = entry.document.upgrade() else {
            return;
        };
        let doc = document.read(cx);
        let mut wanted: Vec<(Attached, Option<ServerSpec>, Option<ExtensionServer>)> = Vec::new();
        let mut asking: Option<(ServerKey, ServerSpec, Vec<ExtensionServer>)> = None;
        if let (Some(path), Some(language)) = (doc.path(), doc.language_name()) {
            entry.uri = path_to_uri(path);
            let brought = crate::extension_store::ExtensionStore::try_global(cx)
                .map(|store| store.read(cx).servers_for(language))
                .unwrap_or_default();
            // The server Solder knows for the language, or one it knows
            // that goes with the extension's.
            let known = spec_for(language).map(|spec| (spec, None)).or_else(|| {
                let (spec, id) = companion(language).filter(|_| !brought.is_empty())?;
                Some((spec, Some(id.to_string())))
            });
            if let Some((spec, language_id)) = known {
                let key = ServerKey {
                    name: spec.name,
                    root: find_root(path, spec.root_markers),
                };
                // Extensions that were not asked yet what they add to it.
                let set_up = self.set_up.entry(key.clone()).or_default();
                let fresh: Vec<ExtensionServer> = brought
                    .iter()
                    .filter(|server| !set_up.asked.contains(&server.id))
                    .cloned()
                    .collect();
                set_up.asked.extend(fresh.iter().map(|s| s.id.clone()));
                if !fresh.is_empty() {
                    asking = Some((key.clone(), spec, fresh));
                }
                wanted.push((Attached::new(key, language_id), Some(spec), None));
            }
            for server in brought {
                let key = ServerKey {
                    name: intern(&server.id),
                    root: find_root(path, &[".git"]),
                };
                // Two extensions that bring the same server start it once.
                if wanted.iter().all(|(attached, ..)| attached.key != key) {
                    let attached = Attached::new(key, server.language_id.clone());
                    wanted.push((attached, None, Some(server)));
                }
            }
            let settings = Settings::get(cx);
            wanted.retain(|(attached, ..)| {
                !settings
                    .language_servers
                    .get(attached.key.name)
                    .is_some_and(|s| s.disabled)
            });
            // Of the servers there are for the language, the ones that
            // start, in the order the user gave them or Zed does.
            let users = settings
                .languages
                .get(language)
                .and_then(|language| language.language_servers.as_deref());
            let choice: Option<Vec<&str>> = match users {
                Some(names) => Some(names.iter().map(String::as_str).collect()),
                None => default_servers(language).map(<[&str]>::to_vec),
            };
            if let Some(choice) = choice {
                let names: Vec<&str> = wanted.iter().map(|(a, ..)| a.key.name).collect();
                let order = chosen_servers(&names, &choice);
                wanted.retain(|(attached, ..)| order.contains(&attached.key.name));
                wanted.sort_by_key(|(attached, ..)| {
                    order.iter().position(|name| *name == attached.key.name)
                });
                // A server that does not start has nothing to be set up.
                asking = asking.filter(|(key, ..)| order.contains(&key.name));
            }
        }
        // A server the document stays in keeps it open; one it leaves is
        // told it closed.
        let before = std::mem::take(&mut entry.servers);
        let mut starting = Vec::new();
        for (mut attached, spec, server) in wanted {
            attached.opened = before
                .iter()
                .any(|was| was.key == attached.key && was.opened);
            starting.push((attached.key.clone(), attached.opened, spec, server));
            entry.servers.push(attached);
        }
        let uri = entry.uri.clone();
        let left: Vec<ServerKey> = before
            .into_iter()
            .filter(|was| was.opened && starting.iter().all(|(key, ..)| *key != was.key))
            .map(|was| was.key)
            .collect();
        for key in &left {
            if let Some(server) = self.server(key) {
                server.notify::<lt::notification::DidCloseTextDocument>(
                    lt::DidCloseTextDocumentParams {
                        text_document: lt::TextDocumentIdentifier { uri: uri.clone() },
                    },
                );
            }
        }
        // What a server it no longer belongs to reported goes with it.
        let doc = document.read(cx);
        let names: Vec<&'static str> = starting.iter().map(|(key, ..)| key.name).collect();
        let stale = doc.diagnostics().iter().any(|d| !names.contains(&d.server));
        if stale {
            document.update(cx, |doc, cx| {
                let kept = doc
                    .diagnostics()
                    .iter()
                    .filter(|d| names.contains(&d.server))
                    .cloned()
                    .collect();
                doc.set_diagnostics(kept, cx);
            });
        }
        for (key, opened, spec, server) in starting {
            // A server that extensions still have something to add to waits
            // for that: it is started, or started again, when they answered.
            let waits = asking.as_ref().is_some_and(|(asked, ..)| *asked == key);
            match (self.servers.get(&key), spec, server) {
                (Some(ServerState::Running { .. }), ..) if opened => {}
                (Some(ServerState::Running { .. }), ..) => self.open(id, &key, cx),
                (Some(ServerState::Starting | ServerState::Failed), ..) => {}
                (None, Some(spec), _) if waits => {
                    self.servers.insert(key, ServerState::Starting);
                    self.status = Some(format!("Starting {}...", spec.name).into());
                    cx.notify();
                }
                (None, Some(spec), _) => self.start(key, spec, cx),
                (None, None, Some(server)) => self.start_from_extension(key, server, cx),
                (None, None, None) => {}
            }
        }
        if let Some((key, spec, servers)) = asking {
            self.set_up_server(key, spec, servers, cx);
        }
    }

    /// Asks the extensions behind `servers` what they add to the options
    /// and settings of a server Solder knows, then starts it with them. If
    /// it runs already and its options changed, it is started again: they
    /// are read once, at the start.
    fn set_up_server(
        &mut self,
        key: ServerKey,
        spec: ServerSpec,
        servers: Vec<ExtensionServer>,
        cx: &mut Context<Self>,
    ) {
        let store = crate::extension_store::ExtensionStore::global(cx);
        let adding = store.update(cx, |store, cx| {
            store.additions(spec.name, servers, &key.root, cx)
        });
        cx.spawn(async move |this, cx| {
            let added = adding.await;
            this.update(cx, |this, cx| {
                let add = |into: &mut Option<serde_json::Value>,
                           more: Option<serde_json::Value>| {
                    match (into.as_mut(), more) {
                        (Some(into), Some(more)) => extension::host::merge_json(into, more),
                        (None, Some(more)) => *into = Some(more),
                        (_, None) => {}
                    }
                };
                let options_changed = added.initialization_options.is_some();
                let settings_changed = added.configuration.is_some();
                let set_up = this.set_up.entry(key.clone()).or_default();
                add(&mut set_up.options, added.initialization_options);
                add(&mut set_up.settings, added.configuration);
                // What the user set for the server stays on top.
                let mut settings = set_up.settings.clone();
                let users = Settings::get(cx)
                    .language_servers
                    .get(key.name)
                    .and_then(|server| server.settings.clone());
                match (settings.as_mut(), users) {
                    (Some(settings), Some(users)) => extension::host::merge_json(settings, users),
                    (None, Some(users)) => settings = Some(users),
                    (_, None) => {}
                }
                match this.servers.get(&key) {
                    Some(ServerState::Running { .. }) if options_changed => {
                        this.restart(key, spec, cx)
                    }
                    Some(ServerState::Running { server, .. }) => {
                        if let (true, Some(settings)) = (settings_changed, settings) {
                            server.set_configuration(settings);
                        }
                    }
                    // Not started yet, or a start that began before the
                    // extensions answered: this one has what they added.
                    _ => this.start(key, spec, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Stops a running server and starts it again with the options it has
    /// now. Its documents are opened in the new one.
    fn restart(&mut self, key: ServerKey, spec: ServerSpec, cx: &mut Context<Self>) {
        if let Some(ServerState::Running { server, .. }) = self.servers.remove(&key) {
            server.kill();
        }
        for entry in self.docs.values_mut() {
            for attached in &mut entry.servers {
                if attached.key == key {
                    attached.opened = false;
                }
            }
        }
        self.start(key, spec, cx);
    }

    /// The extensions that are installed and on changed: every document
    /// joins the servers they now bring for its language and leaves the
    /// ones they no longer do. A document whose servers are the same is not
    /// touched.
    pub fn extension_servers_changed(&mut self, cx: &mut Context<Self>) {
        for id in self.docs.keys().copied().collect::<Vec<_>>() {
            self.attach(id, cx);
        }
    }

    /// Starts a server an extension brings. The extension is asked for the
    /// command first, and may download the server before it answers.
    fn start_from_extension(
        &mut self,
        key: ServerKey,
        server: ExtensionServer,
        cx: &mut Context<Self>,
    ) {
        self.servers.insert(key.clone(), ServerState::Starting);
        self.status = Some(format!("Getting {} ready...", server.name).into());
        cx.notify();
        let store = crate::extension_store::ExtensionStore::global(cx);
        let resolving = store.update(cx, |store, cx| store.resolve(&server, &key.root, cx));
        // What the user set for this server is applied here as well as
        // given to the extension, which may not look at it: their program
        // in place of the extension's, their options and settings on top.
        let overrides = Settings::get(cx)
            .language_servers
            .get(key.name)
            .cloned()
            .unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let result = match resolving.await {
                Ok(mut resolved) => {
                    let on_top =
                        |under: &mut Option<serde_json::Value>, over: Option<serde_json::Value>| {
                            match (under.as_mut(), over) {
                                (Some(under), Some(over)) => {
                                    extension::host::merge_json(under, over)
                                }
                                (None, Some(over)) => *under = Some(over),
                                (_, None) => {}
                            }
                        };
                    on_top(
                        &mut resolved.initialization_options,
                        overrides.initialization_options,
                    );
                    on_top(&mut resolved.configuration, overrides.settings);
                    let command = match overrides.command {
                        Some(program) => ServerCommand {
                            program: PathBuf::from(program),
                            args: overrides.args.unwrap_or(resolved.command.args),
                            env: resolved.command.env,
                        },
                        None => ServerCommand {
                            program: PathBuf::from(resolved.command.command),
                            args: overrides.args.unwrap_or(resolved.command.args),
                            env: resolved.command.env,
                        },
                    };
                    let (name, root) = (key.name, key.root.clone());
                    let spawned = cx
                        .background_executor()
                        .spawn(async move { LanguageServer::spawn(name, &command, &root) })
                        .await;
                    match spawned {
                        Ok((server, notifications)) => server
                            .initialize(&key.root, resolved.initialization_options)
                            .await
                            .map(|_| {
                                if let Some(settings) = resolved.configuration {
                                    server.set_configuration(settings);
                                }
                                (server, notifications)
                            })
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e.to_string()),
                    }
                }
                Err(e) => Err(e),
            };
            this.update(cx, |this, cx| match result {
                Ok((server, notifications)) => this.started(key, server, notifications, cx),
                Err(error) => {
                    this.status = Some(format!("{}: {error}", key.name).into());
                    this.servers.insert(key, ServerState::Failed);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn start(&mut self, key: ServerKey, spec: ServerSpec, cx: &mut Context<Self>) {
        let overrides = Settings::get(cx)
            .language_servers
            .get(spec.name)
            .cloned()
            .unwrap_or_default();
        let program = match overrides.command.as_deref() {
            Some(cmd) => Some(PathBuf::from(cmd)),
            None => find_program(spec.program, &key.root),
        };
        let Some(program) = program else {
            let message = format!(
                "{} not found. Install it: {}",
                spec.program, spec.install_hint
            );
            self.status = Some(message.clone().into());
            self.servers.insert(key, ServerState::Failed);
            cx.notify();
            return;
        };
        let command = ServerCommand {
            program,
            args: overrides
                .args
                .unwrap_or_else(|| spec.args.iter().map(|a| a.to_string()).collect()),
            env: Vec::new(),
        };
        self.servers.insert(key.clone(), ServerState::Starting);
        self.status = Some(format!("Starting {}...", spec.name).into());
        cx.notify();
        // The user's options, and on top of them what extensions add.
        let mut options = overrides.initialization_options;
        let (added, mut settings) = self
            .set_up
            .get(&key)
            .map(|set_up| (set_up.options.clone(), set_up.settings.clone()))
            .unwrap_or_default();
        // The settings the user gave the server, over what extensions add.
        match (settings.as_mut(), overrides.settings) {
            (Some(settings), Some(users)) => extension::host::merge_json(settings, users),
            (None, Some(users)) => settings = Some(users),
            (_, None) => {}
        }
        match (options.as_mut(), added) {
            (Some(options), Some(added)) => extension::host::merge_json(options, added),
            (None, Some(added)) => options = Some(added),
            (_, None) => {}
        }
        let starts = self.starts.entry(key.clone()).or_default();
        *starts += 1;
        let this_start = *starts;
        cx.spawn(async move |this, cx| {
            let root = key.root.clone();
            let spawned = cx
                .background_executor()
                .spawn(async move { LanguageServer::spawn(spec.name, &command, &root) })
                .await;
            let result = match spawned {
                Ok((server, notifications)) => server
                    .initialize(&key.root, options)
                    .await
                    .map(|_| (server, notifications)),
                Err(e) => Err(e),
            };
            this.update(cx, |this, cx| match result {
                // A later start began while this one was on its way: that
                // one has the newer options, and this server is not needed.
                Ok((server, _)) if this.starts.get(&key) != Some(&this_start) => server.kill(),
                Err(_) if this.starts.get(&key) != Some(&this_start) => {}
                Ok((server, notifications)) => {
                    if let Some(settings) = settings {
                        server.set_configuration(settings);
                    }
                    this.started(key, server, notifications, cx)
                }
                Err(err) => {
                    let message = format!(
                        "{} failed to start: {err}. {}",
                        spec.name, spec.install_hint
                    );
                    this.status = Some(message.clone().into());
                    this.servers.insert(key, ServerState::Failed);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn started(
        &mut self,
        key: ServerKey,
        server: Arc<LanguageServer>,
        mut notifications: futures::channel::mpsc::UnboundedReceiver<Notification>,
        cx: &mut Context<Self>,
    ) {
        let pump_key = key.clone();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(n) = notifications.next().await {
                if this
                    .update(cx, |this, cx| this.handle_notification(&pump_key, n, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.status = None;
        self.servers.insert(
            key.clone(),
            ServerState::Running {
                server,
                _notifications: pump,
            },
        );
        let waiting: Vec<EntityId> = self
            .docs
            .iter()
            .filter(|(_, e)| e.servers.iter().any(|a| a.key == key && !a.opened))
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            self.open(id, &key, cx);
        }
        cx.notify();
    }

    fn server(&self, key: &ServerKey) -> Option<&Arc<LanguageServer>> {
        match self.servers.get(key) {
            Some(ServerState::Running { server, .. }) => Some(server),
            _ => None,
        }
    }

    /// The running servers a document is open in, in the order it got them.
    fn opened<'a>(
        &'a self,
        entry: &'a DocEntry,
    ) -> impl Iterator<Item = (&'static str, &'a Arc<LanguageServer>)> + 'a {
        entry
            .servers
            .iter()
            .filter(|attached| attached.opened)
            .filter_map(|attached| Some((attached.key.name, self.server(&attached.key)?)))
    }

    /// Tells the server under `key` that the document is open.
    fn open(&mut self, id: EntityId, key: &ServerKey, cx: &mut Context<Self>) {
        let Some(entry) = self.docs.get(&id) else {
            return;
        };
        let (Some(attached), Some(document)) = (
            entry.servers.iter().find(|a| a.key == *key),
            entry.document.upgrade(),
        ) else {
            return;
        };
        let Some(server) = self.server(key).cloned() else {
            return;
        };
        let doc = document.read(cx);
        server.notify::<lt::notification::DidOpenTextDocument>(lt::DidOpenTextDocumentParams {
            text_document: lt::TextDocumentItem {
                uri: entry.uri.clone(),
                language_id: attached
                    .language_id
                    .clone()
                    .unwrap_or_else(|| doc.language_id().into()),
                // The version the other servers of the document are at: the
                // changes that follow carry the next ones.
                version: entry.version,
                text: doc.text().rope().to_string(),
            },
        });
        if let Some(attached) = self
            .docs
            .get_mut(&id)
            .and_then(|entry| entry.servers.iter_mut().find(|a| a.key == *key))
        {
            attached.opened = true;
        }
    }

    fn did_change(&mut self, id: EntityId, edits: &Arc<[text::Edit]>, cx: &mut Context<Self>) {
        let Some(entry) = self.docs.get_mut(&id) else {
            return;
        };
        if !entry.servers.iter().any(|a| a.opened) {
            return;
        }
        entry.version += 1;
        let Some(entry) = self.docs.get(&id) else {
            return;
        };
        let whole = std::cell::OnceCell::new();
        for (_, server) in self.opened(entry) {
            let sync = sync_kind(&server.capabilities());
            if sync == lt::TextDocumentSyncKind::NONE {
                continue;
            }
            let changes = if sync == lt::TextDocumentSyncKind::INCREMENTAL {
                let utf8 = server.encoding() == Encoding::Utf8;
                edits
                    .iter()
                    .map(|e| {
                        let (start, end) = if utf8 {
                            (e.start_point, e.old_end_point)
                        } else {
                            (e.start_utf16, e.old_end_utf16)
                        };
                        lt::TextDocumentContentChangeEvent {
                            range: Some(lt::Range::new(position(start), position(end))),
                            range_length: None,
                            text: e.new_text.to_string(),
                        }
                    })
                    .collect()
            } else {
                // The whole text, made once for the servers that want it.
                let Some(text) = whole
                    .get_or_init(|| {
                        let document = entry.document.upgrade()?;
                        Some(document.read(cx).text().rope().to_string())
                    })
                    .clone()
                else {
                    continue;
                };
                vec![lt::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text,
                }]
            };
            server.notify::<lt::notification::DidChangeTextDocument>(
                lt::DidChangeTextDocumentParams {
                    text_document: lt::VersionedTextDocumentIdentifier {
                        uri: entry.uri.clone(),
                        version: entry.version,
                    },
                    content_changes: changes,
                },
            );
        }
    }

    fn did_save(&mut self, id: EntityId) {
        let Some(entry) = self.docs.get(&id) else {
            return;
        };
        for (_, server) in self.opened(entry) {
            server.notify::<lt::notification::DidSaveTextDocument>(lt::DidSaveTextDocumentParams {
                text_document: lt::TextDocumentIdentifier {
                    uri: entry.uri.clone(),
                },
                text: None,
            });
        }
    }

    fn close(&mut self, id: EntityId) {
        let Some(entry) = self.docs.get(&id) else {
            return;
        };
        for (_, server) in self.opened(entry) {
            server.notify::<lt::notification::DidCloseTextDocument>(
                lt::DidCloseTextDocumentParams {
                    text_document: lt::TextDocumentIdentifier {
                        uri: entry.uri.clone(),
                    },
                },
            );
        }
        if let Some(entry) = self.docs.get_mut(&id) {
            for attached in &mut entry.servers {
                attached.opened = false;
            }
        }
    }

    fn handle_notification(&mut self, key: &ServerKey, n: Notification, cx: &mut Context<Self>) {
        match n.method.as_str() {
            "textDocument/publishDiagnostics" => {
                let Ok(params) = serde_json::from_value::<lt::PublishDiagnosticsParams>(n.params)
                else {
                    return;
                };
                let Some(server) = self.server(key).cloned() else {
                    return;
                };
                let encoding = server.encoding();
                let name = key.name;
                for entry in self.docs.values() {
                    if entry.uri != params.uri || entry.servers.iter().all(|a| a.key != *key) {
                        continue;
                    }
                    let Some(document) = entry.document.upgrade() else {
                        continue;
                    };
                    let diagnostics = params.diagnostics.clone();
                    document.update(cx, |doc, cx| {
                        // This server's replace its own; what the document's
                        // other servers reported stays.
                        let others = doc
                            .diagnostics()
                            .iter()
                            .filter(|d| d.server != name)
                            .cloned();
                        let fresh = diagnostics.iter().map(|d| Diagnostic {
                            range: from_range(doc.text(), d.range, encoding),
                            severity: match d.severity {
                                Some(lt::DiagnosticSeverity::ERROR) => Severity::Error,
                                Some(lt::DiagnosticSeverity::WARNING) => Severity::Warning,
                                Some(lt::DiagnosticSeverity::HINT) => Severity::Hint,
                                _ => Severity::Info,
                            },
                            message: d.message.clone(),
                            source: d.source.clone(),
                            server: name,
                        });
                        let mut merged: Vec<Diagnostic> = others.chain(fresh).collect();
                        merged.sort_by_key(|d| (d.range.start, d.range.end));
                        doc.set_diagnostics(merged, cx);
                    });
                }
            }
            // Vue's server asking the TypeScript server something: it sends
            // the questions here, each is passed on as a command, and the
            // answers go back the same way.
            "tsserver/request" => {
                let Some(asking) = self.server(key).cloned() else {
                    return;
                };
                // The TypeScript server of a file this server has too.
                let typescript = self
                    .docs
                    .values()
                    .filter(|entry| entry.servers.iter().any(|a| a.key == *key))
                    .flat_map(|entry| &entry.servers)
                    .find(|a| a.key.name == "typescript-language-server")
                    .and_then(|a| self.server(&a.key))
                    .cloned();
                let questions: Vec<(serde_json::Value, String, serde_json::Value)> = n
                    .params
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|question| {
                        Some((
                            question.get(0)?.clone(),
                            question.get(1)?.as_str()?.to_string(),
                            question.get(2).cloned().unwrap_or_default(),
                        ))
                    })
                    .collect();
                cx.background_executor()
                    .spawn(async move {
                        for (id, command, payload) in questions {
                            // Without a TypeScript server the answer is
                            // nothing: Vue's server goes on without it.
                            let body = match &typescript {
                                Some(typescript) => typescript
                                    .request::<lt::request::ExecuteCommand>(
                                        lt::ExecuteCommandParams {
                                            command: "typescript.tsserverRequest".into(),
                                            arguments: vec![command.into(), payload],
                                            work_done_progress_params: Default::default(),
                                        },
                                    )
                                    .await
                                    .ok()
                                    .flatten()
                                    .and_then(|mut answer| answer.get_mut("body").map(|b| b.take()))
                                    .unwrap_or_default(),
                                None => serde_json::Value::Null,
                            };
                            asking.notify::<TsserverResponse>(serde_json::json!([[id, body]]));
                        }
                    })
                    .detach();
            }
            "$/progress" => {
                let Ok(params) = serde_json::from_value::<lt::ProgressParams>(n.params) else {
                    return;
                };
                let lt::ProgressParamsValue::WorkDone(progress) = params.value;
                self.status = match progress {
                    lt::WorkDoneProgress::Begin(b) => Some(progress_text(
                        key.name,
                        &b.title,
                        b.message.as_deref(),
                        b.percentage,
                    )),
                    lt::WorkDoneProgress::Report(r) => Some(progress_text(
                        key.name,
                        "",
                        r.message.as_deref(),
                        r.percentage,
                    )),
                    lt::WorkDoneProgress::End(_) => None,
                };
                cx.notify();
            }
            "workspace/applyEdit" => {
                let Some(server) = self.server(key).cloned() else {
                    return;
                };
                let id = n.id.unwrap_or(serde_json::Value::Null);
                match serde_json::from_value::<lt::ApplyWorkspaceEditParams>(n.params) {
                    Ok(params) => {
                        cx.emit(LspStoreEvent::ApplyEdit {
                            edit: params.edit,
                            encoding: server.encoding(),
                        });
                        server.respond(id, serde_json::json!({ "applied": true }));
                    }
                    Err(e) => server.respond(
                        id,
                        serde_json::json!({ "applied": false, "failureReason": e.to_string() }),
                    ),
                }
            }
            "window/showMessage" => {
                if let Ok(params) = serde_json::from_value::<lt::ShowMessageParams>(n.params)
                    && params.typ == lt::MessageType::ERROR
                {
                    self.status = Some(format!("{}: {}", key.name, params.message).into());
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// Sends a request about `document` to one server: the first of its
    /// servers that said it answers this kind, or the first of all if none
    /// said. `None` when the document has no running server.
    pub fn request<R: lt::request::Request>(
        &self,
        document: &Entity<Document>,
        cx: &App,
        params: impl FnOnce(lt::TextDocumentIdentifier, Encoding, &Buffer) -> R::Params,
    ) -> Option<(Encoding, BoxFuture<'static, lsp::Result<R::Result>>)> {
        let entry = self.docs.get(&document.entity_id())?;
        let server = self
            .opened(entry)
            .map(|(_, server)| server)
            .find(|server| supports(&server.capabilities(), R::METHOD))
            .or_else(|| self.opened(entry).map(|(_, server)| server).next())?;
        let encoding = server.encoding();
        let id = lt::TextDocumentIdentifier {
            uri: entry.uri.clone(),
        };
        let params = params(id, encoding, document.read(cx).text());
        Some((encoding, server.request::<R>(params).boxed()))
    }

    /// Sends a request about `document` to every one of its servers that
    /// answers this kind, for answers that add up: completions, actions.
    /// Each comes with the server's name and its encoding.
    pub fn request_all<R: lt::request::Request>(
        &self,
        document: &Entity<Document>,
        cx: &App,
        params: impl Fn(lt::TextDocumentIdentifier, Encoding, &Buffer) -> R::Params,
    ) -> Vec<Asked<R>> {
        let Some(entry) = self.docs.get(&document.entity_id()) else {
            return Vec::new();
        };
        let buffer = document.read(cx).text();
        self.opened(entry)
            .filter(|(_, server)| supports(&server.capabilities(), R::METHOD))
            .map(|(name, server)| {
                let encoding = server.encoding();
                let id = lt::TextDocumentIdentifier {
                    uri: entry.uri.clone(),
                };
                let request = server.request::<R>(params(id, encoding, buffer)).boxed();
                (name, encoding, request)
            })
            .collect()
    }

    /// Asks for the symbols of `document`: the first of its servers that
    /// lists them, by name, since the extension that brought it may have
    /// a way to paint them. `None` when none does.
    pub fn document_symbols(
        &self,
        document: &Entity<Document>,
    ) -> Option<Asked<lt::request::DocumentSymbolRequest>> {
        let entry = self.docs.get(&document.entity_id())?;
        let (name, server) = self.opened(entry).find(|(_, server)| {
            // Said outright: a server that says nothing of it has none.
            server.capabilities().document_symbol_provider.is_some()
                && supports(&server.capabilities(), "textDocument/documentSymbol")
        })?;
        let params = lt::DocumentSymbolParams {
            text_document: lt::TextDocumentIdentifier {
                uri: entry.uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let request = server
            .request::<lt::request::DocumentSymbolRequest>(params)
            .boxed();
        Some((name, server.encoding(), request))
    }

    /// Asks every running server of the project in `root` that lists the
    /// project's symbols for the ones that match `query`.
    pub fn workspace_symbols(
        &self,
        root: &Path,
        query: &str,
    ) -> Vec<Asked<lt::request::WorkspaceSymbolRequest>> {
        self.servers
            .iter()
            .filter(|(key, _)| key.root.starts_with(root) || root.starts_with(&key.root))
            .filter_map(|(key, state)| {
                let ServerState::Running { server, .. } = state else {
                    return None;
                };
                let caps = server.capabilities();
                if caps.workspace_symbol_provider.is_none() || !supports(&caps, "workspace/symbol")
                {
                    return None;
                }
                let params = lt::WorkspaceSymbolParams {
                    query: query.to_string(),
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                };
                let request = server
                    .request::<lt::request::WorkspaceSymbolRequest>(params)
                    .boxed();
                Some((key.name, server.encoding(), request))
            })
            .collect()
    }

    /// Sends a request to one of the document's servers by name, one that is
    /// not about a position in the document (code action resolve, execute
    /// command): it goes back to the server the action came from.
    pub fn server_request<R: lt::request::Request>(
        &self,
        document: &Entity<Document>,
        server: &'static str,
        params: R::Params,
    ) -> Option<BoxFuture<'static, lsp::Result<R::Result>>> {
        let entry = self.docs.get(&document.entity_id())?;
        let (_, server) = self.opened(entry).find(|(name, _)| *name == server)?;
        Some(server.request::<R>(params).boxed())
    }

    /// What each of the document's running servers can do.
    pub fn all_capabilities(&self, document: &Entity<Document>) -> Vec<lt::ServerCapabilities> {
        let Some(entry) = self.docs.get(&document.entity_id()) else {
            return Vec::new();
        };
        self.opened(entry)
            .map(|(_, server)| server.capabilities())
            .collect()
    }

    /// Stops every server. Called when the app quits.
    pub fn shutdown_all(&mut self) -> Vec<Arc<LanguageServer>> {
        self.servers
            .drain()
            .filter_map(|(_, s)| match s {
                ServerState::Running { server, .. } => Some(server),
                _ => None,
            })
            .collect()
    }
}

fn progress_text(
    name: &str,
    title: &str,
    message: Option<&str>,
    percent: Option<u32>,
) -> SharedString {
    let mut text = format!("{name}: {title}");
    if let Some(m) = message {
        if !title.is_empty() {
            text.push(' ');
        }
        text.push_str(m);
    }
    if let Some(p) = percent {
        text.push_str(&format!(" {p}%"));
    }
    text.into()
}

fn sync_kind(caps: &lt::ServerCapabilities) -> lt::TextDocumentSyncKind {
    match &caps.text_document_sync {
        Some(lt::TextDocumentSyncCapability::Kind(k)) => *k,
        Some(lt::TextDocumentSyncCapability::Options(o)) => {
            o.change.unwrap_or(lt::TextDocumentSyncKind::NONE)
        }
        None => lt::TextDocumentSyncKind::NONE,
    }
}

fn position(p: Point) -> lt::Position {
    lt::Position::new(p.row as u32, p.column as u32)
}

pub fn to_position(buffer: &Buffer, offset: usize, encoding: Encoding) -> lt::Position {
    match encoding {
        Encoding::Utf8 => position(buffer.offset_to_point(offset)),
        Encoding::Utf16 => position(buffer.offset_to_utf16(offset)),
    }
}

pub fn to_offset(buffer: &Buffer, pos: lt::Position, encoding: Encoding) -> usize {
    let point = Point::new(pos.line as usize, pos.character as usize);
    match encoding {
        Encoding::Utf8 => buffer.point_to_offset(point),
        Encoding::Utf16 => buffer.utf16_to_offset(point),
    }
}

pub fn from_range(buffer: &Buffer, range: lt::Range, encoding: Encoding) -> Range<usize> {
    let start = to_offset(buffer, range.start, encoding);
    let end = to_offset(buffer, range.end, encoding);
    start.min(end)..end.max(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_servers_of_a_language_are_the_ones_chosen() {
        let ruby = [
            "fuzzy-ruby-server",
            "herb",
            "kanayago",
            "rubocop",
            "ruby-lsp",
            "solargraph",
            "sorbet",
            "steep",
        ];
        // As Zed chooses for Ruby: one of the eight, and those it does
        // not name. An extension that brings a ninth starts it.
        let defaults = default_servers("Ruby").unwrap();
        assert_eq!(chosen_servers(&ruby, defaults), ["solargraph", "herb"]);
        // The user's own: named ones first, in their order, then the rest
        // less the ones left out.
        assert_eq!(
            chosen_servers(
                &ruby,
                &["ruby-lsp", "!solargraph", "!herb", "rubocop", "..."]
            )[..3],
            ["ruby-lsp", "rubocop", "fuzzy-ruby-server"]
        );
        // Without `...` only the named start; one that is not there, or
        // is named twice, changes nothing.
        assert_eq!(
            chosen_servers(&ruby, &["sorbet", "pyright", "sorbet", "steep"]),
            ["sorbet", "steep"]
        );
        // `...` keeps the order servers come in, and none at all is none.
        assert_eq!(chosen_servers(&["a", "b", "c"], &["...", "!b"]), ["a", "c"]);
        assert_eq!(
            chosen_servers(&["a", "b", "c"], &["c", "..."]),
            ["c", "a", "b"]
        );
        assert!(chosen_servers(&ruby, &[]).is_empty());
        // A language Solder has a server of its own for has no choice
        // made for it.
        assert!(default_servers("Rust").is_none() && default_servers("TypeScript").is_none());
    }

    #[test]
    fn root_is_nearest_marker() {
        let dir = db::testing::dir("root");
        std::fs::create_dir_all(dir.join("crates/a/src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("crates/a/Cargo.toml"), "").unwrap();
        let file = dir.join("crates/a/src/lib.rs");
        assert_eq!(find_root(&file, &["Cargo.toml"]), dir.join("crates/a"));
        assert_eq!(find_root(&file, &["go.mod"]), dir.join("crates/a/src"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn positions_convert_in_both_encodings() {
        let b = Buffer::new("é😀x\nab");
        // "x" is byte 6, UTF-16 column 3.
        assert_eq!(to_position(&b, 6, Encoding::Utf8), lt::Position::new(0, 6));
        assert_eq!(to_position(&b, 6, Encoding::Utf16), lt::Position::new(0, 3));
        assert_eq!(to_offset(&b, lt::Position::new(0, 3), Encoding::Utf16), 6);
        assert_eq!(to_offset(&b, lt::Position::new(1, 1), Encoding::Utf8), 9);
    }
}
