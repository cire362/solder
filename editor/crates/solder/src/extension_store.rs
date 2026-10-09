//! Extensions made for Zed and VS Code: what is installed, the two catalogs
//! they come from, and what the editor takes from them (languages with their
//! grammars, snippets, themes).
//!
//! Nothing here runs at startup beyond reading the manifests of what is
//! installed, off the UI thread. A grammar is compiled when a file of its
//! language is opened, and the network is used only when the Extensions
//! window searches or installs.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use extension::{
    Entry, Event, Extension, Origin, Refusals, Snippet, catalog,
    gate::{Did, Gate},
    host::{CodeLabel, Completion, DebugAdapter, DebugLaunch, Host, Status, Symbol, World},
    install::{self, Progress, Staged},
    world::{SettingsFor, System},
};
use futures::{StreamExt, channel::mpsc};
use gpui::{App, AppContext, Context, Entity, Global, SharedString, Task, WeakEntity};

use crate::{
    document::Document,
    file_icons::FileIcons,
    import_settings,
    lsp_store::LspStore,
    settings::{ServerOverride, Settings},
};

/// An extension by where it is from and its id in lowercase: the two
/// catalogs do not agree with the manifests on the case of ids.
pub type Key = (Origin, String);

pub fn key(origin: Origin, id: &str) -> Key {
    (origin, id.to_lowercase())
}

/// One catalog as the Extensions window last saw it.
#[derive(Default)]
pub struct Catalog {
    pub query: String,
    pub entries: Vec<Entry>,
    pub error: Option<String>,
    pub searching: bool,
    /// True once a search has answered, so "nothing found" is not shown
    /// before one was made.
    pub searched: bool,
    generation: usize,
}

/// A language server an installed extension knows how to get and start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionServer {
    /// The extension's id.
    pub extension: String,
    pub id: String,
    pub name: String,
    /// What the server calls the language, where the extension says.
    pub language_id: Option<String>,
}

/// What an extension answered about its server.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub command: extension::host::Command,
    pub initialization_options: Option<serde_json::Value>,
    pub configuration: Option<serde_json::Value>,
}

/// What installed extensions add to a language server that is not theirs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Additions {
    pub initialization_options: Option<serde_json::Value>,
    pub configuration: Option<serde_json::Value>,
}

/// An extension's code, loaded when a server of it is first needed. The
/// lock is held while it loads, so two projects asking at once load it once.
type Slot = Arc<Mutex<Option<Arc<Host>>>>;

/// The snippets of one file and the languages they are for.
struct SnippetSet {
    /// The extension the file belongs to.
    owner: Key,
    languages: Vec<String>,
    snippets: Vec<Snippet>,
}

pub struct ExtensionStore {
    /// Where extensions are kept: `zed/<id>` and `vscode/<publisher.name>`.
    pub root: PathBuf,
    /// Where settings and themes are kept.
    config: PathBuf,
    pub installed: Vec<Extension>,
    /// False until the first scan has finished.
    pub loaded: bool,
    snippets: Vec<SnippetSet>,
    /// The languages last given to the syntax registry.
    languages: Vec<syntax::LanguageSpec>,
    zed: Catalog,
    open_vsx: Catalog,
    /// The catalogs' addresses; tests point them at a local server.
    pub zed_url: String,
    pub open_vsx_url: String,
    pub installing: HashMap<Key, Arc<Progress>>,
    /// What other extensions needed that was already looked for, by id
    /// in small letters.
    sought: std::collections::HashSet<String>,
    /// Downloaded and read, waiting for the user to allow what they would
    /// do outside a sandbox.
    pub pending: HashMap<Key, Staged>,
    /// Newer versions the catalogs have of what is installed.
    pub updates: HashMap<Key, Entry>,
    /// What the user decided: turned off, kept on a version, allowed.
    state: extension::State,
    /// Why the last install or removal of an extension failed.
    pub errors: HashMap<Key, String>,
    /// What "Use" last did with a theme.
    pub theme_status: Option<Result<String, String>>,
    /// The same for the last icon theme chosen.
    pub icon_status: Option<Result<String, String>>,
    documents: Vec<WeakEntity<Document>>,
    scans: usize,
    /// The loaded code of extensions, by extension id.
    hosts: HashMap<String, Slot>,
    /// By extension id. Kept when the code is loaded again, so what an
    /// extension did is not forgotten with it.
    gates: HashMap<String, Gated>,
    /// The extension servers that were asked for their command already, by
    /// extension and server id: that is when an extension installs what it
    /// needs, and it comes before anything else is asked of it.
    resolved: Arc<Mutex<Vec<(String, String)>>>,
    /// What extensions reach outside their sandbox through. `None` is the
    /// real thing; tests script it.
    pub world: Option<Arc<dyn World>>,
    /// Where extensions report on the servers they are getting ready.
    statuses: mpsc::UnboundedSender<(String, Status)>,
    /// The user's settings as extensions ask for them. They ask from their
    /// own threads, so this is a copy, renewed when the settings change.
    asked: Arc<Mutex<Asked>>,
    _settings: gpui::Subscription,
    _pump: Task<()>,
}

/// What stands between one extension and the world: what the user took
/// back from it, which the gate reads at every call, and what it did.
#[derive(Clone, Default)]
struct Gated {
    refusals: Arc<Mutex<Refusals>>,
    did: Did,
}

/// The loaded code of an extension: what its slot holds, or loaded now.
/// Blocking, and slow the first time.
/// Puts `value` where the dots of `key` lead: `a.b.c` is `c` in `b` in
/// `a`. An object put where one is already is laid over it, name by name,
/// so that `"a": { "b": 1 }` and `"a.c": 2` in one file both hold.
fn put_at(into: &mut serde_json::Value, key: &str, value: serde_json::Value) {
    let mut at = into;
    for part in key.split('.').filter(|part| !part.is_empty()) {
        if !at.is_object() {
            *at = serde_json::json!({});
        }
        at = &mut at[part];
    }
    match (at.is_object(), value) {
        (true, serde_json::Value::Object(fields)) => {
            for (name, field) in fields {
                put_at(at, &name, field);
            }
        }
        (_, value) => *at = value,
    }
}

/// The launch a declared debugger is given: the one its manifest suggests,
/// with its places filled in for the file to debug, or with none
/// suggested, the least any adapter is told.
fn launch_of(
    debugger: &extension::Debugger,
    launch: &DebugLaunch,
    root: &Path,
) -> serde_json::Value {
    let file = Path::new(&launch.program);
    let part = |part: Option<&std::ffi::OsStr>| {
        part.map(|part| part.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let relative = file
        .strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string();
    let places = [
        ("${file}", launch.program.clone()),
        ("${relativeFile}", relative.clone()),
        ("${fileBasenameNoExtension}", part(file.file_stem())),
        ("${fileBasename}", part(file.file_name())),
        ("${fileDirname}", part(file.parent().map(Path::as_os_str))),
        ("${workspaceFolder}", root.display().to_string()),
        ("${workspaceRoot}", root.display().to_string()),
    ];
    fn fill(value: &mut serde_json::Value, places: &[(&str, String)], asked: &str) {
        match value {
            serde_json::Value::String(text) => {
                for (place, with) in places {
                    *text = text.replace(place, with);
                }
                // What VS Code would ask the user for is the file: it
                // is the file in front that is being debugged.
                while let Some(start) = text.find("${command:").or(text.find("${input:")) {
                    let end = text[start..]
                        .find('}')
                        .map_or(text.len(), |end| start + end + 1);
                    text.replace_range(start..end, asked);
                }
            }
            serde_json::Value::Array(items) => {
                items.iter_mut().for_each(|item| fill(item, places, asked))
            }
            serde_json::Value::Object(fields) => fields
                .values_mut()
                .for_each(|field| fill(field, places, asked)),
            _ => {}
        }
    }
    let mut config = debugger
        .initial
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    fill(&mut config, &places, &relative);
    config["type"] = debugger.name.clone().into();
    config["request"] = "launch".into();
    config["name"] = launch.label.clone().into();
    if config["program"].is_null() {
        config["program"] = launch.program.clone().into();
    }
    if config["cwd"].is_null() {
        config["cwd"] = launch
            .cwd
            .clone()
            .unwrap_or_else(|| root.display().to_string())
            .into();
    }
    if !launch.args.is_empty() {
        config["args"] = launch.args.clone().into();
    }
    config
}

/// What an extension reaches outside through: the world tests give, or
/// the real one, behind the gate of what the user took back from it.
fn world_in(
    work_dir: &Path,
    world: Option<Arc<dyn World>>,
    statuses: mpsc::UnboundedSender<(String, Status)>,
    settings: SettingsFor,
    gated: Gated,
) -> Arc<Gate> {
    let world = world.unwrap_or_else(|| {
        let mut system = System::new(extension::world::user_env(), move |server, status| {
            let _ = statuses.unbounded_send((server.to_string(), status));
        });
        system.settings = Some(settings);
        // Next to the extensions: their work folders are `work/<id>` under
        // the same root.
        system.node_home = work_dir.ancestors().nth(2).map(install::node_dir);
        Arc::new(system)
    });
    Arc::new(Gate::new(world, gated.refusals, gated.did))
}

fn host_in(
    slot: &Slot,
    extension: &Extension,
    work_dir: &Path,
    world: Option<Arc<dyn World>>,
    statuses: mpsc::UnboundedSender<(String, Status)>,
    settings: SettingsFor,
    gated: Gated,
) -> Result<Arc<Host>, String> {
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(host) = &*slot {
        return Ok(host.clone());
    }
    let world = world_in(work_dir, world, statuses, settings, gated);
    let host = Arc::new(Host::load(extension, work_dir, world)?);
    *slot = Some(host.clone());
    Ok(host)
}

/// What of the user's settings extensions may ask for.
#[derive(Default)]
struct Asked {
    servers: std::collections::BTreeMap<String, ServerOverride>,
    indent: usize,
    /// What the user set for a context server, by the server's name.
    context: std::collections::BTreeMap<String, serde_json::Value>,
}

struct GlobalExtensionStore(Entity<ExtensionStore>);

impl Global for GlobalExtensionStore {}

impl ExtensionStore {
    pub fn new(root: PathBuf, config: PathBuf, cx: &mut Context<Self>) -> Self {
        // An extension speaks from its own thread; what it says about a
        // server goes to the status bar, next to the language servers' own.
        let (statuses, mut said) = mpsc::unbounded::<(String, Status)>();
        let pump = cx.spawn(async move |_, cx| {
            while let Some((server, status)) = said.next().await {
                let text: Option<SharedString> = match status {
                    Status::Ready => None,
                    Status::CheckingForUpdate => {
                        Some(format!("Checking {server} for updates...").into())
                    }
                    Status::Downloading => Some(format!("Downloading {server}...").into()),
                    Status::Failed(why) => Some(format!("{server}: {why}").into()),
                };
                let shown = cx.update(|cx| {
                    if let Some(lsp) = LspStore::global(cx) {
                        lsp.update(cx, |lsp, cx| lsp.set_status(text, cx));
                    }
                });
                if shown.is_err() {
                    break;
                }
            }
        });
        let asked = Arc::new(Mutex::new(Asked::default()));
        let copy = |asked: &Mutex<Asked>, cx: &App| {
            if let Some(settings) = cx.try_global::<Settings>() {
                *asked.lock().unwrap_or_else(|e| e.into_inner()) = Asked {
                    servers: settings.language_servers.clone(),
                    indent: settings.indent_size,
                    context: settings
                        .context_servers
                        .iter()
                        .filter_map(|(name, server)| Some((name.clone(), server.settings.clone()?)))
                        .collect(),
                };
            }
        };
        copy(&asked, cx);
        let watched = asked.clone();
        let watching = cx.observe_global::<Settings>(move |this, cx| {
            copy(&watched, cx);
            this.sync_icons(cx);
        });
        Self {
            root,
            config,
            installed: Vec::new(),
            loaded: false,
            snippets: Vec::new(),
            languages: Vec::new(),
            zed: Catalog::default(),
            open_vsx: Catalog::default(),
            zed_url: catalog::ZED.into(),
            open_vsx_url: catalog::OPEN_VSX.into(),
            installing: HashMap::new(),
            sought: Default::default(),
            pending: HashMap::new(),
            updates: HashMap::new(),
            state: extension::State::default(),
            errors: HashMap::new(),
            theme_status: None,
            icon_status: None,
            documents: Vec::new(),
            scans: 0,
            hosts: HashMap::new(),
            gates: HashMap::new(),
            resolved: Arc::default(),
            world: None,
            statuses,
            asked,
            _settings: watching,
            _pump: pump,
        }
    }

    /// The app's store; the first call reads what is installed, off the UI
    /// thread.
    pub fn global(cx: &mut App) -> Entity<ExtensionStore> {
        if let Some(store) = cx.try_global::<GlobalExtensionStore>() {
            return store.0.clone();
        }
        // Tests never see the user's own extensions or settings.
        #[cfg(test)]
        let (root, config) = (
            db::testing::dir("no-extensions"),
            db::testing::dir("no-extensions-config"),
        );
        #[cfg(not(test))]
        let (root, config) = (
            dirs::data_dir()
                .unwrap_or_else(crate::settings::config_dir)
                .join("Solder/extensions"),
            crate::settings::config_dir(),
        );
        let store = cx.new(|cx| ExtensionStore::new(root, config, cx));
        cx.set_global(GlobalExtensionStore(store.clone()));
        store.update(cx, |s, cx| s.scan(cx));
        store
    }

    /// The user's settings in the form an extension asks for them: for a
    /// server, by the name it asks with, what `language_servers` has under
    /// that name; for a language, the indent. Nothing set gives `None`.
    pub fn settings_for(&self) -> SettingsFor {
        let asked = self.asked.clone();
        Arc::new(move |category, key| {
            let asked = asked.lock().unwrap_or_else(|e| e.into_inner());
            match category {
                "lsp" => {
                    let server = asked.servers.get(key?)?;
                    Some(extension::world::lsp_settings(
                        server.command.as_deref(),
                        server.args.as_deref(),
                        server.initialization_options.as_ref(),
                        server.settings.as_ref(),
                    ))
                }
                "language" if asked.indent > 0 => {
                    Some(extension::world::language_settings(asked.indent))
                }
                // In the shape Zed's API gives an extension: its own
                // command is not the user's to set here, its settings are.
                "context_servers" => {
                    let settings = asked.context.get(key?)?;
                    Some(serde_json::json!({ "command": null, "settings": settings }).to_string())
                }
                _ => None,
            }
        })
    }

    pub fn try_global(cx: &App) -> Option<Entity<ExtensionStore>> {
        cx.try_global::<GlobalExtensionStore>().map(|g| g.0.clone())
    }

    #[cfg(test)]
    pub fn set_global(store: Entity<ExtensionStore>, cx: &mut App) {
        cx.set_global(GlobalExtensionStore(store));
    }

    /// Keeps a document's language in step with what is installed.
    pub fn register(document: &Entity<Document>, cx: &mut App) {
        let store = Self::global(cx);
        store.update(cx, |store, _| {
            store.documents.retain(|d| d.upgrade().is_some());
            store.documents.push(document.downgrade());
        });
    }

    pub fn catalog(&self, origin: Origin) -> &Catalog {
        match origin {
            Origin::Zed => &self.zed,
            Origin::VsCode => &self.open_vsx,
        }
    }

    fn catalog_mut(&mut self, origin: Origin) -> &mut Catalog {
        match origin {
            Origin::Zed => &mut self.zed,
            Origin::VsCode => &mut self.open_vsx,
        }
    }

    pub fn find(&self, origin: Origin, id: &str) -> Option<&Extension> {
        self.installed
            .iter()
            .find(|e| e.origin == origin && e.id.eq_ignore_ascii_case(id))
    }

    /// Reads what is installed, then hands its languages to the syntax
    /// registry and its snippets to completion.
    pub fn scan(&mut self, cx: &mut Context<Self>) {
        self.scans += 1;
        let scan = self.scans;
        let root = self.root.clone();
        // The decisions are read from disk once; after that the store has
        // the newer ones and writes them back.
        let first = !self.loaded;
        cx.spawn(async move |this, cx| {
            let (installed, snippets, state) = cx
                .background_executor()
                .spawn(async move {
                    let installed = install::installed(&root);
                    let snippets: Vec<SnippetSet> = installed
                        .iter()
                        .flat_map(|extension| {
                            let owner = key(extension.origin, &extension.id);
                            extension
                                .snippets
                                .iter()
                                .map(move |file| (owner.clone(), file))
                        })
                        .filter_map(|(owner, file)| {
                            let text = std::fs::read_to_string(&file.path).ok()?;
                            Some(SnippetSet {
                                owner,
                                languages: file.languages.clone(),
                                snippets: extension::snippet::parse(&text),
                            })
                        })
                        .filter(|set| !set.snippets.is_empty())
                        .collect();
                    let state = first.then(|| extension::State::load(&root));
                    (installed, snippets, state)
                })
                .await;
            this.update(cx, |this, cx| {
                // A later scan has the newer picture.
                if this.scans != scan {
                    return;
                }
                // An extension that changed or went away takes its loaded
                // code with it; the next request loads what is there now.
                this.hosts.retain(|id, _| {
                    let before = this.installed.iter().find(|e| e.id == *id);
                    before.is_some() && before == installed.iter().find(|e| e.id == *id)
                });
                let loaded: Vec<&String> = this.hosts.keys().collect();
                this.resolved
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|(extension, _)| loaded.contains(&extension));
                if let (Some(state), false) = (state, this.loaded) {
                    this.state = state;
                    // A gate made before the decisions were read.
                    for (id, gated) in &this.gates {
                        *gated.refusals.lock().unwrap_or_else(|e| e.into_inner()) =
                            this.state.refusals(Origin::Zed, id);
                    }
                }
                this.installed = installed;
                this.snippets = snippets;
                this.loaded = true;
                // What was updated, removed or kept back is no update.
                let (installed, state) = (&this.installed, &this.state);
                this.updates.retain(|(origin, id), entry| {
                    !state.is_kept(*origin, id)
                        && installed.iter().any(|e| {
                            e.origin == *origin
                                && e.id.eq_ignore_ascii_case(id)
                                && e.version != entry.version
                        })
                });
                this.sync_languages(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Puts the icon theme the settings name in use, if an extension that
    /// is installed and on has it.
    fn sync_icons(&mut self, cx: &mut Context<Self>) {
        let wanted = cx
            .try_global::<Settings>()
            .and_then(|settings| settings.icon_theme.clone());
        let theme = wanted.and_then(|name| {
            self.installed
                .iter()
                .filter(|extension| !self.is_off(extension.origin, &extension.id))
                .flat_map(|extension| &extension.icon_themes)
                .find(|theme| theme.name == name)
                .cloned()
        });
        let now = cx
            .try_global::<FileIcons>()
            .and_then(|icons| icons.0.as_deref());
        if now != theme.as_ref() {
            cx.set_global(FileIcons(theme.map(Arc::new)));
            cx.refresh_windows();
        }
    }

    /// Makes `name` the icon theme, in `settings.json`.
    pub fn use_icon_theme(&mut self, name: String, cx: &mut Context<Self>) {
        let config = self.config.clone();
        cx.spawn(async move |this, cx| {
            let dir = config.clone();
            let chosen = name.clone();
            let applied = cx
                .background_executor()
                .spawn(async move {
                    let choice = import_settings::Choice {
                        settings: vec![import::Setting {
                            key: "icon_theme",
                            label: "Icon theme",
                            value: chosen.into(),
                        }],
                        ..Default::default()
                    };
                    import_settings::apply(&dir, &choice)
                })
                .await;
            this.update(cx, |this, cx| {
                this.icon_status = Some(applied.map(|_| name));
                // As with a theme: at once, not when the watcher notices.
                crate::settings::reload_from(&config, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn sync_languages(&mut self, cx: &mut Context<Self>) {
        self.sync_icons(cx);
        let specs: Vec<syntax::LanguageSpec> = self
            .installed
            .iter()
            .filter(|extension| !self.is_off(extension.origin, &extension.id))
            .flat_map(|extension| &extension.languages)
            .filter_map(|language| {
                let grammar = language.grammar.as_ref()?;
                Some(syntax::LanguageSpec {
                    name: language.name.clone(),
                    suffixes: language.suffixes.clone(),
                    aliases: language.aliases.clone(),
                    line_comment: language.line_comment.clone(),
                    symbol: grammar.symbol.clone(),
                    grammar: grammar.module.clone(),
                    highlights: grammar.highlights.clone(),
                    injections: grammar.injections.clone(),
                    editing: syntax::Editing {
                        pairs: language
                            .pairs
                            .iter()
                            .map(|pair| syntax::Pair {
                                start: pair.start.clone(),
                                end: pair.end.clone(),
                                close: pair.close,
                                newline: pair.newline,
                                not_in: pair.not_in.clone(),
                            })
                            .collect(),
                        autoclose_before: language.autoclose_before.clone(),
                        block_comment: language.block_comment.clone(),
                        word_characters: language.word_characters.clone(),
                        completion_characters: language.completion_characters.clone(),
                        increase_indent: language.increase_indent.clone(),
                        decrease_indent: language.decrease_indent.clone(),
                        indents: grammar.indents.clone(),
                        brackets: grammar.brackets.clone(),
                        outline: grammar.outline.clone(),
                        overrides: grammar.overrides.clone(),
                    },
                })
            })
            .collect();
        // The servers extensions bring may have changed with them, also
        // for languages Solder has on its own. Told once this update is
        // over: the documents ask this store which servers are theirs.
        cx.defer(|cx| {
            if let Some(lsp) = LspStore::global(cx) {
                lsp.update(cx, |lsp, cx| lsp.extension_servers_changed(cx));
            }
        });
        if specs == self.languages {
            return;
        }
        self.languages = specs.clone();
        syntax::set_extension_languages(specs);
        self.documents.retain(|d| d.upgrade().is_some());
        for document in self.documents.clone() {
            document
                .update(cx, |document, cx| document.languages_changed(cx))
                .ok();
        }
    }

    /// The snippets for a language that goes by any of `ids` (lowercase).
    pub fn snippets_for(&self, ids: &[String]) -> Vec<&Snippet> {
        let applies = |languages: &[String]| {
            languages.is_empty() || languages.iter().any(|l| ids.contains(l))
        };
        self.snippets
            .iter()
            .filter(|set| !self.state.is_off(set.owner.0, &set.owner.1))
            .filter(|set| applies(&set.languages))
            .flat_map(|set| &set.snippets)
            .filter(|snippet| applies(&snippet.scopes))
            .collect()
    }

    /// The servers installed extensions bring for `language`: every one
    /// that lists it, of every extension that is on and whose code Solder
    /// runs, in the order the extensions and their manifests give.
    pub fn servers_for(&self, language: &str) -> Vec<ExtensionServer> {
        self.installed
            .iter()
            .filter(|extension| extension.runs_code())
            .filter(|extension| !self.is_off(extension.origin, &extension.id))
            .flat_map(|extension| {
                extension
                    .servers
                    .iter()
                    .filter(|server| server.languages.iter().any(|l| l == language))
                    .map(|server| ExtensionServer {
                        extension: extension.id.clone(),
                        id: server.id.clone(),
                        name: server.name.clone(),
                        language_id: server
                            .language_ids
                            .iter()
                            .find(|(name, _)| name == language)
                            .map(|(_, id)| id.clone()),
                    })
            })
            .collect()
    }

    /// The context servers installed extensions bring: the extension's id
    /// and the server's name.
    pub fn context_servers(&self) -> Vec<(String, String)> {
        self.installed
            .iter()
            .filter(|extension| {
                extension.runs_code() && !self.is_off(extension.origin, &extension.id)
            })
            .flat_map(|extension| {
                extension
                    .context_servers
                    .iter()
                    .map(|server| (extension.id.clone(), server.clone()))
            })
            .collect()
    }

    /// Asks the extension how to start its context server. It may install
    /// the server first, so this runs on a thread of its own.
    pub fn context_server_command(
        &mut self,
        extension: &str,
        server: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<extension::host::Command, String>> {
        let Some(extension) = self.find(Origin::Zed, extension).cloned() else {
            return Task::ready(Err(format!("{extension} is not installed")));
        };
        let slot = self.hosts.entry(extension.id.clone()).or_default().clone();
        let work_dir = install::work_dir(&self.root, &extension.id);
        let world = self.world.clone();
        let statuses = self.statuses.clone();
        let settings = self.settings_for();
        let gated = self.gated(&extension.id);
        let server = server.to_string();
        let (tx, rx) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("solder-extension".into())
            .spawn(move || {
                let answer = host_in(
                    &slot, &extension, &work_dir, world, statuses, settings, gated,
                )
                .and_then(|host| host.context_server_command(&server));
                let _ = tx.send(answer);
            });
        if let Err(error) = spawned {
            return Task::ready(Err(error.to_string()));
        }
        cx.background_executor().spawn(async move {
            rx.await
                .unwrap_or_else(|_| Err("The extension stopped without an answer".into()))
        })
    }

    /// The debug adapters installed extensions bring for `language`: the
    /// extension's id and the adapter's name. The language says which
    /// adapters debug it; an extension that has one of them is asked.
    pub fn debuggers_for(&self, language: &str) -> Vec<(String, String)> {
        let on = |extension: &&Extension| {
            extension.runs_code() && !self.is_off(extension.origin, &extension.id)
        };
        let wanted: Vec<&String> = self
            .installed
            .iter()
            .filter(on)
            .flat_map(|extension| &extension.languages)
            .filter(|known| known.name == language)
            .flat_map(|known| &known.debuggers)
            .collect();
        self.installed
            .iter()
            .filter(on)
            .flat_map(|extension| {
                extension
                    .debug_adapters
                    .iter()
                    .filter(|adapter| wanted.contains(adapter))
                    .map(|adapter| (extension.id.clone(), adapter.clone()))
            })
            .collect()
    }

    /// What the settings of installed extensions are under `section`
    /// (`prettier`, or `editor.suggest`; all of them for an empty one):
    /// what their manifests declare by default, with what the user set in
    /// settings.json over it. An object, as VS Code hands one to an
    /// extension that asks for its configuration.
    pub fn configuration(&self, section: &str, cx: &App) -> serde_json::Value {
        let mut all = serde_json::json!({});
        let declared = self
            .installed
            .iter()
            .filter(|e| !self.is_off(e.origin, &e.id))
            .flat_map(|extension| &extension.settings);
        for setting in declared {
            if !setting.default.is_null() {
                put_at(&mut all, &setting.key, setting.default.clone());
            }
        }
        if let Some(settings) = cx.try_global::<Settings>() {
            for (key, value) in &settings.other {
                put_at(&mut all, key, value.clone());
            }
        }
        section
            .split('.')
            .filter(|part| !part.is_empty())
            .try_fold(&all, |at, part| at.get(part))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    }

    /// The debug adapters there are for `file`: the extension's id and
    /// the adapter's name. Those of Zed extensions go by the file's
    /// language, if it has one here. Those a VS Code extension declares
    /// go by the languages they name: one that is the file's language, or
    /// one some installed extension says files so named are of.
    pub fn debuggers_for_file(&self, file: &Path, language: Option<&str>) -> Vec<(String, String)> {
        let mut found = language
            .map(|language| self.debuggers_for(language))
            .unwrap_or_default();
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let on: Vec<&Extension> = self
            .installed
            .iter()
            .filter(|e| e.origin == Origin::VsCode && !self.is_off(e.origin, &e.id))
            .collect();
        // The ids of the languages a file of this name is of.
        let ids: Vec<String> = on
            .iter()
            .flat_map(|extension| &extension.languages)
            .filter(|known| {
                known
                    .suffixes
                    .iter()
                    .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")))
            })
            .filter_map(|known| known.aliases.first())
            .map(|id| id.to_lowercase())
            .chain(language.map(str::to_lowercase))
            .collect();
        for extension in on {
            for debugger in &extension.debuggers {
                if debugger.languages.iter().any(|id| ids.contains(id)) {
                    found.push((extension.id.clone(), debugger.name.clone()));
                }
            }
        }
        found
    }

    /// How to start an adapter a VS Code extension declares, with no
    /// code of the extension's run: the program its manifest names, by
    /// the runtime it names, and the launch it suggests with its places
    /// filled in. Finding the runtime may take a download (Node, where
    /// the machine has none), so this runs on a thread of its own.
    fn declared_adapter(
        &mut self,
        id: &str,
        launch: DebugLaunch,
        root: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<DebugAdapter, String>> {
        let found = self.find(Origin::VsCode, id).and_then(|extension| {
            let debugger = extension
                .debuggers
                .iter()
                .find(|debugger| debugger.name == launch.adapter)?;
            Some((extension.clone(), debugger.clone()))
        });
        let Some((extension, debugger)) = found else {
            return Task::ready(Err(format!("{id} has no debugger to start")));
        };
        let work_dir = install::work_dir(&self.root, &extension.id);
        let world = self.world.clone();
        let statuses = self.statuses.clone();
        let settings = self.settings_for();
        let gated = self.gated(&extension.id);
        let root = root.to_path_buf();
        let (tx, rx) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("solder-extension".into())
            .spawn(move || {
                let program = debugger.program.to_string_lossy().into_owned();
                let command = match debugger.runtime.as_deref() {
                    None => Ok((program, debugger.args.clone())),
                    Some(runtime) => {
                        let runtime = match runtime {
                            // The machine's Node, or one of Solder's own.
                            "node" => world_in(&work_dir, world, statuses, settings, gated).node(),
                            other => Ok(other.to_string()),
                        };
                        runtime.map(|runtime| {
                            let mut args = debugger.runtime_args.clone();
                            args.push(program);
                            args.extend(debugger.args.clone());
                            (runtime, args)
                        })
                    }
                };
                let answer = command.map(|(command, args)| DebugAdapter {
                    command: Some(command),
                    args,
                    env: Vec::new(),
                    cwd: Some(extension.dir.to_string_lossy().into_owned()),
                    connection: None,
                    attach: false,
                    configuration: launch_of(&debugger, &launch, &root).to_string(),
                });
                let _ = tx.send(answer);
            });
        if let Err(error) = spawned {
            return Task::ready(Err(error.to_string()));
        }
        cx.background_executor().spawn(async move {
            rx.await
                .unwrap_or_else(|_| Err("The debugger was not found".into()))
        })
    }

    /// Asks the extension how to start its debug adapter for `launch`. It
    /// may install the adapter first, so this runs on a thread of its own,
    /// like [`ExtensionStore::resolve`].
    pub fn debug_adapter(
        &mut self,
        extension: &str,
        launch: DebugLaunch,
        root: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<DebugAdapter, String>> {
        let Some(extension) = self.find(Origin::Zed, extension).cloned() else {
            // One a VS Code extension declares needs no code to ask.
            return self.declared_adapter(extension, launch, root, cx);
        };
        let slot = self.hosts.entry(extension.id.clone()).or_default().clone();
        let work_dir = install::work_dir(&self.root, &extension.id);
        let world = self.world.clone();
        let statuses = self.statuses.clone();
        let settings = self.settings_for();
        let gated = self.gated(&extension.id);
        let root = root.to_path_buf();
        let (tx, rx) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("solder-extension".into())
            .spawn(move || {
                let answer = host_in(
                    &slot, &extension, &work_dir, world, statuses, settings, gated,
                )
                .and_then(|host| host.debug_adapter(&launch, &root));
                let _ = tx.send(answer);
            });
        if let Err(error) = spawned {
            return Task::ready(Err(error.to_string()));
        }
        cx.background_executor().spawn(async move {
            rx.await
                .unwrap_or_else(|_| Err("The extension stopped without an answer".into()))
        })
    }

    /// How the extension that brought `server` wants these completions of
    /// it shown, one answer for each. `None` when no extension's code is
    /// running for that server: one is not started for a menu's sake, and
    /// a server that runs was started by its extension. An extension that
    /// is busy, or fails, paints nothing.
    pub fn labels(
        &self,
        server: &str,
        completions: Vec<Completion>,
        cx: &App,
    ) -> Option<Task<Vec<Option<CodeLabel>>>> {
        let name = server.to_string();
        self.painted(server, cx, move |host| {
            host.labels_for_completions(&name, &completions)
        })
    }

    /// The same for the symbols `server` lists.
    pub fn symbol_labels(
        &self,
        server: &str,
        symbols: Vec<Symbol>,
        cx: &App,
    ) -> Option<Task<Vec<Option<CodeLabel>>>> {
        let name = server.to_string();
        self.painted(server, cx, move |host| {
            host.labels_for_symbols(&name, &symbols)
        })
    }

    /// Asks the extension that brought `server` for labels, on a thread
    /// of its own.
    fn painted(
        &self,
        server: &str,
        cx: &App,
        ask: impl FnOnce(&Host) -> Option<Result<Vec<Option<CodeLabel>>, String>> + Send + 'static,
    ) -> Option<Task<Vec<Option<CodeLabel>>>> {
        let extension = self.installed.iter().find(|extension| {
            extension.runs_code()
                && !self.is_off(extension.origin, &extension.id)
                && extension.servers.iter().any(|s| s.id == server)
        })?;
        let host = self
            .hosts
            .get(&extension.id)?
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let (tx, rx) = futures::channel::oneshot::channel();
        std::thread::Builder::new()
            .name("solder-extension".into())
            .spawn(move || {
                let labels = ask(&host).and_then(Result::ok).unwrap_or_default();
                let _ = tx.send(labels);
            })
            .ok()?;
        Some(
            cx.background_executor()
                .spawn(async move { rx.await.unwrap_or_default() }),
        )
    }

    /// Asks the extension how to start `server` for the project in `root`.
    /// The extension may first download the server, so this can take as
    /// long as that does. It runs on a thread of its own.
    pub fn resolve(
        &mut self,
        server: &ExtensionServer,
        root: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<Resolved, String>> {
        let Some(extension) = self.find(Origin::Zed, &server.extension).cloned() else {
            return Task::ready(Err(format!("{} is not installed", server.extension)));
        };
        let slot = self.hosts.entry(extension.id.clone()).or_default().clone();
        let work_dir = install::work_dir(&self.root, &extension.id);
        let world = self.world.clone();
        let statuses = self.statuses.clone();
        let resolved = self.resolved.clone();
        let settings = self.settings_for();
        let gated = self.gated(&extension.id);
        let (id, root) = (server.id.clone(), root.to_path_buf());
        let (tx, rx) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("solder-extension".into())
            .spawn(move || {
                let answer = (|| {
                    let host = host_in(
                        &slot,
                        &extension,
                        &work_dir,
                        world,
                        statuses,
                        settings,
                        gated.clone(),
                    )?;
                    let json = |text: Option<String>| {
                        text.and_then(|text| serde_json::from_str(&text).ok())
                    };
                    let command = host.language_server_command(&id, &root)?;
                    resolved
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((extension.id.clone(), id.clone()));
                    Ok(Resolved {
                        command,
                        initialization_options: json(host.initialization_options(&id, &root)?),
                        configuration: json(host.workspace_configuration(&id, &root)?),
                    })
                })();
                // A server that could not be got ready is part of what the
                // extension did, in the words the status bar had.
                if let Err(error) = &answer {
                    gated
                        .did
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(Event::Failed(format!("{id}: {error}")));
                }
                let _ = tx.send(answer);
            });
        if let Err(error) = spawned {
            return Task::ready(Err(error.to_string()));
        }
        cx.background_executor().spawn(async move {
            rx.await
                .unwrap_or_else(|_| Err("The extension stopped without an answer".into()))
        })
    }

    /// What the extensions behind `servers` add to the options and settings
    /// of `target`, a server that is not theirs. Each is asked for its own
    /// server's command first if it was not yet: that is when it installs
    /// what the addition points to. Runs on a thread of its own, and an
    /// extension that fails adds nothing.
    pub fn additions(
        &mut self,
        target: &str,
        servers: Vec<ExtensionServer>,
        root: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Additions> {
        let mut asked = Vec::new();
        for server in servers {
            let Some(extension) = self.find(Origin::Zed, &server.extension).cloned() else {
                continue;
            };
            let slot = self.hosts.entry(extension.id.clone()).or_default().clone();
            let work_dir = install::work_dir(&self.root, &extension.id);
            let gated = self.gated(&extension.id);
            asked.push((server.id, extension, slot, work_dir, gated));
        }
        let world = self.world.clone();
        let statuses = self.statuses.clone();
        let resolved = self.resolved.clone();
        let settings = self.settings_for();
        let (target, root) = (target.to_string(), root.to_path_buf());
        let (tx, rx) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("solder-extension".into())
            .spawn(move || {
                let mut additions = Additions::default();
                let add = |into: &mut Option<serde_json::Value>, text: Option<String>| {
                    let Some(more) = text.and_then(|t| serde_json::from_str(&t).ok()) else {
                        return;
                    };
                    match into {
                        Some(into) => extension::host::merge_json(into, more),
                        None => *into = Some(more),
                    }
                };
                for (id, extension, slot, work_dir, gated) in asked {
                    let Ok(host) = host_in(
                        &slot,
                        &extension,
                        &work_dir,
                        world.clone(),
                        statuses.clone(),
                        settings.clone(),
                        gated,
                    ) else {
                        continue;
                    };
                    let seen = (extension.id.clone(), id.clone());
                    let known = resolved
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains(&seen);
                    if !known {
                        if host.language_server_command(&id, &root).is_err() {
                            continue;
                        }
                        resolved
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(seen);
                    }
                    if let Ok(options) = host.additional_initialization_options(&id, &target, &root)
                    {
                        add(&mut additions.initialization_options, options);
                    }
                    if let Ok(settings) =
                        host.additional_workspace_configuration(&id, &target, &root)
                    {
                        add(&mut additions.configuration, settings);
                    }
                }
                let _ = tx.send(additions);
            });
        if spawned.is_err() {
            return Task::ready(Additions::default());
        }
        cx.background_executor()
            .spawn(async move { rx.await.unwrap_or_default() })
    }

    /// Asks a catalog for `query`. An answer to an older question is dropped.
    pub fn search(&mut self, origin: Origin, query: &str, cx: &mut Context<Self>) {
        let base = match origin {
            Origin::Zed => self.zed_url.clone(),
            Origin::VsCode => self.open_vsx_url.clone(),
        };
        let catalog = self.catalog_mut(origin);
        catalog.generation += 1;
        catalog.query = query.to_string();
        catalog.searching = true;
        catalog.error = None;
        let generation = catalog.generation;
        let answer = catalog::search(origin, &base, query);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| {
                let catalog = this.catalog_mut(origin);
                if catalog.generation != generation {
                    return;
                }
                catalog.searching = false;
                catalog.searched = true;
                match answer {
                    Ok(entries) => catalog.entries = entries,
                    Err(error) => {
                        catalog.entries.clear();
                        catalog.error = Some(error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Downloads `entry` and installs it over an older copy. If it would do
    /// something outside a sandbox that it was not allowed before, it waits
    /// in `pending` for [`Self::allow`] or [`Self::refuse`].
    pub fn install(&mut self, entry: Entry, cx: &mut Context<Self>) {
        let key = key(entry.origin, &entry.id);
        if self.installing.contains_key(&key) {
            return;
        }
        let progress = Progress::new();
        self.installing.insert(key.clone(), progress.clone());
        self.errors.remove(&key);
        // A download of the same extension takes the staging folder of one
        // that was waiting.
        self.pending.remove(&key);
        cx.notify();
        let fetching = install::fetch(entry, self.root.clone(), progress);
        // The download reports through atomics; repaint while it runs.
        let ticking = key.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                let running = this.update(cx, |this, cx| {
                    cx.notify();
                    this.installing.contains_key(&ticking)
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let fetched = fetching.await;
            this.update(cx, |this, cx| {
                this.installing.remove(&key);
                match fetched {
                    Ok(staged) if this.state.asks(&staged.extension).is_empty() => {
                        this.commit(key, staged, cx)
                    }
                    Ok(staged) => {
                        this.pending.insert(key, staged);
                    }
                    Err(error) => {
                        this.errors.insert(key, error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Puts a fetched extension in place and reads the folder again.
    fn commit(&mut self, key: Key, staged: Staged, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { install::commit(staged, &root) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(extension) => {
                        this.updates.remove(&key);
                        this.install_needed(&extension, cx);
                    }
                    Err(error) => {
                        this.errors.insert(key, error);
                    }
                }
                this.scan(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Installs what an extension that was just installed does not work
    /// without, and what it is a pack of. Each is looked up once: one the
    /// catalog does not have, such as a part of VS Code itself, is left
    /// out, and two that need each other end where they began.
    fn install_needed(&mut self, extension: &Extension, cx: &mut Context<Self>) {
        let wanted: Vec<String> = extension
            .needs
            .iter()
            .filter(|id| !id.to_lowercase().starts_with("vscode."))
            .filter(|id| {
                !self.installed.iter().any(|known| {
                    known.origin == extension.origin && known.id.eq_ignore_ascii_case(id)
                })
            })
            .filter(|id| self.sought.insert(id.to_lowercase()))
            .cloned()
            .collect();
        if wanted.is_empty() {
            return;
        }
        let origin = extension.origin;
        let base = match origin {
            Origin::Zed => self.zed_url.clone(),
            Origin::VsCode => self.open_vsx_url.clone(),
        };
        let asking = catalog::latest(origin, &base, wanted);
        cx.spawn(async move |this, cx| {
            let Ok(entries) = asking.await else {
                return;
            };
            this.update(cx, |this, cx| {
                for entry in entries {
                    this.install(entry, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// What the extension waiting under this id would do outside a sandbox
    /// that it was not allowed yet.
    pub fn asks(&self, origin: Origin, id: &str) -> Vec<String> {
        self.pending
            .get(&key(origin, id))
            .map(|staged| self.state.asks(&staged.extension))
            .unwrap_or_default()
    }

    /// The user read what the waiting extension would do and agreed.
    pub fn allow(&mut self, origin: Origin, id: &str, cx: &mut Context<Self>) {
        let key = key(origin, id);
        let Some(staged) = self.pending.remove(&key) else {
            return;
        };
        self.state.allow(&staged.extension);
        self.save_state(cx);
        self.commit(key, staged, cx);
        cx.notify();
    }

    /// The user did not agree: the download is dropped.
    pub fn refuse(&mut self, origin: Origin, id: &str, cx: &mut Context<Self>) {
        if let Some(staged) = self.pending.remove(&key(origin, id)) {
            cx.background_executor()
                .spawn(async move { install::discard(staged) })
                .detach();
            cx.notify();
        }
    }

    /// Writes the decisions next to the extensions, off the UI thread.
    fn save_state(&self, cx: &mut Context<Self>) {
        let (state, root) = (self.state.clone(), self.root.clone());
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = state.save(&root) {
                    eprintln!("extensions: could not save state.json: {error}");
                }
            })
            .detach();
    }

    fn gated(&mut self, id: &str) -> Gated {
        let refused = self.state.refusals(Origin::Zed, id);
        self.gates
            .entry(id.to_string())
            .or_insert_with(|| Gated {
                refusals: Arc::new(Mutex::new(refused)),
                did: Did::default(),
            })
            .clone()
    }

    /// What the user took back from the code of the Zed extension `id`.
    pub fn refusals(&self, id: &str) -> Refusals {
        self.state.refusals(Origin::Zed, id)
    }

    /// Takes something back from an extension's code, or gives it back.
    /// It holds from the next thing the extension asks for: what it
    /// already started keeps running.
    pub fn set_refusals(&mut self, id: &str, refused: Refusals, cx: &mut Context<Self>) {
        if let Some(gated) = self.gates.get(id) {
            *gated.refusals.lock().unwrap_or_else(|e| e.into_inner()) = refused.clone();
        }
        self.state.set_refusals(Origin::Zed, id, refused);
        self.save_state(cx);
        cx.notify();
    }

    /// What the code of the Zed extension `id` did outside its sandbox
    /// since the app started, and what it was refused.
    pub fn did(&self, id: &str) -> Vec<Event> {
        self.gates.get(id).map_or_else(Vec::new, |gated| {
            gated
                .did
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned()
                .collect()
        })
    }

    pub fn is_off(&self, origin: Origin, id: &str) -> bool {
        self.state.is_off(origin, id)
    }

    /// Turns an extension off or on without removing it. Off, its languages,
    /// snippets and servers are not used.
    pub fn set_off(&mut self, origin: Origin, id: &str, off: bool, cx: &mut Context<Self>) {
        self.state.set_off(origin, id, off);
        self.save_state(cx);
        if let Some(extension) = self.find(origin, id) {
            let id = extension.id.clone();
            self.hosts.remove(&id);
        }
        self.sync_languages(cx);
        cx.notify();
    }

    pub fn is_kept(&self, origin: Origin, id: &str) -> bool {
        self.state.is_kept(origin, id)
    }

    /// Keeps an extension on the version it has, or lets it follow updates.
    pub fn set_kept(&mut self, origin: Origin, id: &str, kept: bool, cx: &mut Context<Self>) {
        self.state.set_kept(origin, id, kept);
        self.save_state(cx);
        if kept {
            self.updates.remove(&key(origin, id));
        } else {
            self.check_updates(cx);
        }
        cx.notify();
    }

    /// Asks the catalogs for the newest versions of what is installed and
    /// not kept back. Called when the Extensions tab is opened.
    pub fn check_updates(&mut self, cx: &mut Context<Self>) {
        for origin in [Origin::Zed, Origin::VsCode] {
            // A catalog knows an extension by the name of its folder.
            let ids: Vec<String> = self
                .installed
                .iter()
                .filter(|e| e.origin == origin && !self.state.is_kept(origin, &e.id))
                .filter_map(|e| Some(e.dir.file_name()?.to_string_lossy().into_owned()))
                .collect();
            if ids.is_empty() {
                continue;
            }
            let base = match origin {
                Origin::Zed => self.zed_url.clone(),
                Origin::VsCode => self.open_vsx_url.clone(),
            };
            let asking = catalog::latest(origin, &base, ids);
            cx.spawn(async move |this, cx| {
                // A catalog that cannot be reached offers no updates; the
                // list above already says so when it is searched.
                let Ok(newest) = asking.await else {
                    return;
                };
                this.update(cx, |this, cx| {
                    for entry in newest {
                        let newer = this
                            .find(origin, &entry.id)
                            .is_some_and(|installed| installed.version != entry.version);
                        if newer && !this.state.is_kept(origin, &entry.id) {
                            this.updates.insert(key(origin, &entry.id), entry);
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Installs every update there is.
    pub fn update_all(&mut self, cx: &mut Context<Self>) {
        for entry in self.updates.values().cloned().collect::<Vec<_>>() {
            self.install(entry, cx);
        }
    }

    pub fn cancel(&mut self, origin: Origin, id: &str) {
        if let Some(progress) = self.installing.get(&key(origin, id)) {
            progress.cancel();
        }
    }

    pub fn remove(&mut self, origin: Origin, id: &str, cx: &mut Context<Self>) {
        // The folder is named as the catalog names the extension, which may
        // differ from the manifest's id in case.
        let Some(folder) = self
            .find(origin, id)
            .and_then(|e| e.dir.file_name())
            .map(|name| name.to_string_lossy().into_owned())
        else {
            return;
        };
        let key = key(origin, id);
        let root = self.root.clone();
        // Installed again later, it is a first install: on, following
        // updates, and asked what it may do.
        self.state.forget(origin, id);
        if origin == Origin::Zed {
            self.gates.remove(id);
        }
        self.updates.remove(&key);
        self.save_state(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { install::remove(&root, origin, &folder) })
                .await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.errors.insert(key, error);
                }
                this.scan(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Saves one of an extension's themes in the themes folder and makes it
    /// the editor's theme.
    pub fn use_theme(&mut self, theme: import::ThemeFile, cx: &mut Context<Self>) {
        let config = self.config.clone();
        let name = theme.name.clone();
        cx.spawn(async move |this, cx| {
            let dir = config.clone();
            let applied = cx
                .background_executor()
                .spawn(async move {
                    let choice = import_settings::Choice {
                        theme: Some(theme),
                        ..Default::default()
                    };
                    import_settings::apply(&dir, &choice)
                })
                .await;
            this.update(cx, |this, cx| {
                this.theme_status = Some(applied.map(|_| name));
                // The settings watcher would do this a moment later; doing
                // it here shows the theme at once.
                crate::settings::reload_from(&config, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
