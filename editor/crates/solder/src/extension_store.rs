//! Extensions made for Zed and VS Code: what is installed, the two catalogs
//! they come from, and what the editor takes from them (languages with their
//! grammars, snippets, themes).
//!
//! Nothing here runs at startup beyond reading the manifests of what is
//! installed, off the UI thread. A grammar is compiled when a file of its
//! language is opened, and the network is used only when the Extensions
//! window searches or installs.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use extension::{
    Entry, Extension, Origin, Snippet, catalog,
    install::{self, Progress},
};
use gpui::{App, AppContext, Context, Entity, Global, WeakEntity};

use crate::{document::Document, import_settings};

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

/// The snippets of one file and the languages they are for.
struct SnippetSet {
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
    /// Why the last install or removal of an extension failed.
    pub errors: HashMap<Key, String>,
    /// What "Use" last did with a theme.
    pub theme_status: Option<Result<String, String>>,
    documents: Vec<WeakEntity<Document>>,
    scans: usize,
}

struct GlobalExtensionStore(Entity<ExtensionStore>);

impl Global for GlobalExtensionStore {}

impl ExtensionStore {
    pub fn new(root: PathBuf, config: PathBuf) -> Self {
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
            errors: HashMap::new(),
            theme_status: None,
            documents: Vec::new(),
            scans: 0,
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
        let store = cx.new(|_| ExtensionStore::new(root, config));
        cx.set_global(GlobalExtensionStore(store.clone()));
        store.update(cx, |s, cx| s.scan(cx));
        store
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
        cx.spawn(async move |this, cx| {
            let (installed, snippets) = cx
                .background_executor()
                .spawn(async move {
                    let installed = install::installed(&root);
                    let snippets: Vec<SnippetSet> = installed
                        .iter()
                        .flat_map(|extension| &extension.snippets)
                        .filter_map(|file| {
                            let text = std::fs::read_to_string(&file.path).ok()?;
                            Some(SnippetSet {
                                languages: file.languages.clone(),
                                snippets: extension::snippet::parse(&text),
                            })
                        })
                        .filter(|set| !set.snippets.is_empty())
                        .collect();
                    (installed, snippets)
                })
                .await;
            this.update(cx, |this, cx| {
                // A later scan has the newer picture.
                if this.scans != scan {
                    return;
                }
                this.installed = installed;
                this.snippets = snippets;
                this.loaded = true;
                this.sync_languages(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn sync_languages(&mut self, cx: &mut Context<Self>) {
        let specs: Vec<syntax::LanguageSpec> = self
            .installed
            .iter()
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
                })
            })
            .collect();
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
            .filter(|set| applies(&set.languages))
            .flat_map(|set| &set.snippets)
            .filter(|snippet| applies(&snippet.scopes))
            .collect()
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

    /// Downloads and installs `entry`, replacing an older copy.
    pub fn install(&mut self, entry: Entry, cx: &mut Context<Self>) {
        let key = key(entry.origin, &entry.id);
        if self.installing.contains_key(&key) {
            return;
        }
        let progress = Progress::new();
        self.installing.insert(key.clone(), progress.clone());
        self.errors.remove(&key);
        cx.notify();
        let installing = install::install(entry, self.root.clone(), progress);
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
            let result = installing.await;
            this.update(cx, |this, cx| {
                this.installing.remove(&key);
                match result {
                    Ok(_) => this.scan(cx),
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
