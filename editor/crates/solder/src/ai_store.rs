//! Local models in the app: what this machine can run, the benchmark,
//! downloads, models added by hand and which model serves each role. One
//! store for all windows, since models are shared and only one server should
//! hold memory.
//!
//! Nothing runs until the AI tab is opened.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use ai::{
    Candidate, Dirs, Hardware, Model, Progress, Role, Source, Speed, catalog, custom, install,
    keys::Keys,
};
use gpui::{App, AppContext, Context, Entity, Global, SharedString, Task};

use crate::ai_providers::{self, LOCAL, ModelRef, ProviderInfo, Running, Status, Urls};

/// What the benchmark is doing.
#[derive(Clone)]
pub enum BenchStep {
    Runtime(Arc<Progress>),
    Model(Arc<Progress>),
    Measuring,
}

pub struct AiStore {
    pub dirs: Dirs,
    hub: String,
    /// Folders searched for models other apps downloaded.
    scan_dirs: Vec<PathBuf>,
    pub loaded: bool,
    pub hardware: Option<Hardware>,
    pub free_disk: Option<u64>,
    /// The calibration model's speed on this machine.
    pub measured: Option<Speed>,
    /// Speeds measured on each installed model after its download.
    pub verified: HashMap<String, Speed>,
    /// The model each task uses, local or from a provider.
    pub roles: HashMap<Role, ModelRef>,
    pub providers: Vec<ProviderInfo>,
    /// Only models on this machine (and providers on localhost) answer.
    pub offline: bool,
    pub(crate) keys: Keys,
    /// The local server chat is using, and the model it is starting.
    pub(crate) local: Vec<Running>,
    pub starting: Option<String>,
    /// Suggest code at the cursor while typing, when a model has the task.
    pub completions: bool,
    /// Local models found unable to fill in the middle; asked through chat.
    pub no_infill: std::collections::HashSet<String>,
    /// Open projects, for the `.solderignore` that covers a file.
    pub roots: Vec<PathBuf>,
    /// Models added by hand, kept in `ai.json`.
    pub custom: Vec<Model>,
    /// Models found in LM Studio's folders, read again on each start.
    pub found: Vec<Model>,
    pub installed: Vec<String>,
    pub benchmark: Option<BenchStep>,
    pub downloads: HashMap<String, Arc<Progress>>,
    /// Models being measured after their download.
    pub verifying: Vec<String>,
    /// A model being looked up to add.
    pub adding: bool,
    pub error: Option<SharedString>,
    ticker: Option<Task<()>>,
}

struct GlobalAiStore(Entity<AiStore>);

impl Global for GlobalAiStore {}

fn role_key(role: Role) -> &'static str {
    match role {
        Role::Chat => "chat",
        Role::Completion => "completion",
    }
}

/// `~/models/x.gguf` and `/abs/x.gguf` are paths; anything else is looked
/// up on Hugging Face.
fn local_path(input: &str) -> Option<PathBuf> {
    let input = input.trim();
    let path = match input.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()?.join(rest),
        None => PathBuf::from(input),
    };
    (path.is_absolute() || input.ends_with(".gguf") && path.exists()).then_some(path)
}

impl AiStore {
    pub fn new(dirs: Dirs, hub: String) -> Self {
        let keys = Keys::new(dirs.root.join("keys.json"));
        Self {
            dirs,
            hub,
            scan_dirs: custom::lm_studio_dirs(),
            loaded: false,
            hardware: None,
            free_disk: None,
            measured: None,
            verified: HashMap::new(),
            roles: HashMap::new(),
            providers: ai_providers::builtin(&Urls::default()),
            offline: false,
            keys,
            local: Vec::new(),
            starting: None,
            completions: true,
            no_infill: Default::default(),
            roots: Vec::new(),
            custom: Vec::new(),
            found: Vec::new(),
            installed: Vec::new(),
            benchmark: None,
            downloads: HashMap::new(),
            verifying: Vec::new(),
            adding: false,
            error: None,
            ticker: None,
        }
    }

    /// The app's store, created on first use.
    pub fn global(cx: &mut App) -> Entity<AiStore> {
        if let Some(store) = cx.try_global::<GlobalAiStore>() {
            return store.0.clone();
        }
        let root = dirs::data_dir()
            .unwrap_or_else(crate::settings::config_dir)
            .join("Solder");
        let store = cx.new(|_| AiStore::new(Dirs::new(root), install::HUB.into()));
        cx.on_app_quit({
            let store = store.clone();
            move |cx| {
                // The server is a child process; it would outlive the editor.
                store.update(cx, |s, _| s.stop_local());
                async {}
            }
        })
        .detach();
        cx.set_global(GlobalAiStore(store.clone()));
        store
    }

    /// The store, if AI has been used in this run; never creates it.
    pub fn try_global(cx: &App) -> Option<Entity<AiStore>> {
        cx.try_global::<GlobalAiStore>().map(|g| g.0.clone())
    }

    #[cfg(test)]
    pub fn set_global(store: Entity<AiStore>, cx: &mut App) {
        cx.set_global(GlobalAiStore(store));
    }

    #[cfg(test)]
    pub fn with_scan_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.scan_dirs = dirs;
        self
    }

    /// Keys in a file and providers at other addresses, so tests touch
    /// neither the Keychain nor the network.
    #[cfg(test)]
    pub fn for_tests(mut self, urls: Urls) -> Self {
        self.keys = Keys::file_only(self.dirs.root.join("keys.json"));
        self.providers = ai_providers::builtin(&urls);
        self
    }

    /// Remembers an open project, whose rules apply to its files.
    pub fn add_root(&mut self, root: PathBuf) {
        if !self.roots.contains(&root) {
            self.roots.push(root);
        }
    }

    /// The deepest open project holding `path`.
    pub fn root_for(&self, path: &std::path::Path) -> Option<PathBuf> {
        self.roots
            .iter()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.components().count())
            .cloned()
    }

    pub fn set_completions(&mut self, on: bool, cx: &mut Context<Self>) {
        self.completions = on;
        self.save(cx);
        cx.notify();
    }

    /// The local provider lists what is installed.
    fn sync_local(&mut self) {
        let installed = self.installed.clone();
        if let Some(local) = self.providers.iter_mut().find(|p| p.id == LOCAL) {
            local.status = Status::Ready(installed);
        }
    }

    /// The catalog, then models added by hand, then models found on disk.
    pub fn models(&self) -> Vec<Model> {
        let mut out: Vec<Model> = catalog::models().to_vec();
        for m in self.custom.iter().chain(&self.found) {
            if !out.iter().any(|o| o.id == m.id) {
                out.push(m.clone());
            }
        }
        out
    }

    /// Reads the machine and what is installed, the first time.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let dirs = self.dirs.clone();
        let scan_dirs = self.scan_dirs.clone();
        let read = cx.background_executor().spawn(async move {
            let hw = ai::hardware::detect();
            let free = ai::free_space(&dirs.root);
            let state = std::fs::read(dirs.state())
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
            let found: Vec<Model> = custom::scan(&scan_dirs)
                .iter()
                .filter_map(|p| custom::from_file(p).ok())
                .collect();
            (hw, free, state, found)
        });
        cx.spawn(async move |this, cx| {
            let (hw, free, state, found) = read.await;
            this.update(cx, |this, cx| {
                this.hardware = Some(hw);
                this.free_disk = free;
                this.found = found;
                if let Some(state) = &state {
                    this.measured = Speed::from_json(&state["calibration"]);
                    if let Some(map) = state["verified"].as_object() {
                        for (id, speed) in map {
                            if let Some(s) = Speed::from_json(speed) {
                                this.verified.insert(id.clone(), s);
                            }
                        }
                    }
                    this.custom = state["custom"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Model::from_json)
                        .collect();
                    this.offline = state["offline"].as_bool().unwrap_or(false);
                    this.completions = state["completions"].as_bool().unwrap_or(true);
                    for p in state["providers"].as_array().into_iter().flatten() {
                        if let Some(p) = ProviderInfo::compatible_from_json(p)
                            && this.provider(&p.id).is_none()
                        {
                            this.providers.push(p);
                        }
                    }
                }
                this.installed = install::installed_models(&this.dirs, &this.models());
                this.sync_local();
                if let Some(state) = &state {
                    for role in [Role::Chat, Role::Completion] {
                        let Some(model) = ModelRef::from_json(&state["roles"][role_key(role)])
                        else {
                            continue;
                        };
                        // A local model deleted outside Solder is no choice.
                        if model.provider != LOCAL || this.installed.contains(&model.model) {
                            this.roles.insert(role, model);
                        }
                    }
                }
                this.refresh_providers(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn save(&self, cx: &mut Context<Self>) {
        let mut roles = serde_json::Map::new();
        for (role, model) in &self.roles {
            roles.insert(role_key(*role).into(), model.to_json());
        }
        let verified: serde_json::Map<String, serde_json::Value> = self
            .verified
            .iter()
            .map(|(id, s)| (id.clone(), s.to_json()))
            .collect();
        let state = serde_json::json!({
            "calibration": self.measured.map(Speed::to_json),
            "verified": verified,
            "roles": roles,
            "custom": self.custom.iter().map(Model::to_json).collect::<Vec<_>>(),
            "providers": self
                .providers
                .iter()
                .filter(|p| p.kind == ai_providers::Kind::Compatible)
                .map(ProviderInfo::to_json)
                .collect::<Vec<_>>(),
            "offline": self.offline,
            "completions": self.completions,
        });
        let path = self.dirs.state();
        cx.background_executor()
            .spawn(async move {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(path, serde_json::to_vec_pretty(&state).unwrap_or_default());
            })
            .detach();
    }

    pub fn candidates(&self) -> Vec<Candidate> {
        match &self.hardware {
            Some(hw) => catalog::recommend(hw, self.measured, &self.verified, &self.models()),
            None => Vec::new(),
        }
    }

    pub fn busy(&self) -> bool {
        self.benchmark.is_some() || !self.downloads.is_empty() || !self.verifying.is_empty()
    }

    /// Redraws while something is downloading, so progress moves.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let busy = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        let busy = this.busy();
                        if !busy {
                            this.ticker = None;
                        }
                        busy
                    })
                    .unwrap_or(false);
                if !busy {
                    break;
                }
            }
        }));
    }

    /// The server binary, downloading it first if needed.
    async fn runtime(dirs: Dirs, hw: Hardware, progress: Arc<Progress>) -> Result<PathBuf, String> {
        if let Some(path) = install::installed_runtime(&dirs) {
            return Ok(path);
        }
        let asset = install::runtime_asset(&hw)
            .ok_or_else(|| format!("No llama.cpp build for {} {}", hw.os, hw.arch))?;
        ai::spawn(install::install_runtime(dirs, asset, progress)).await
    }

    /// Fetches the server and the calibration model if missing, then times
    /// the calibration model.
    pub fn run_benchmark(&mut self, cx: &mut Context<Self>) {
        let Some(hw) = self.hardware.clone() else {
            return;
        };
        if self.benchmark.is_some() {
            return;
        }
        self.error = None;
        let dirs = self.dirs.clone();
        let hub = self.hub.clone();
        let progress = Progress::new();
        // Measurements need the memory a chat model holds.
        self.stop_local();
        self.benchmark = Some(BenchStep::Runtime(progress.clone()));
        self.tick(cx);
        cx.spawn(async move |this, cx| {
            let model = catalog::calibration();
            let result: Result<Speed, String> = async {
                let binary = Self::runtime(dirs.clone(), hw, progress).await?;
                let path = dirs.model(model);
                if !path.is_file() {
                    let progress = Progress::new();
                    this.update(cx, |this, _| {
                        this.benchmark = Some(BenchStep::Model(progress.clone()))
                    })
                    .ok();
                    ai::spawn(install::install_model(
                        dirs.clone(),
                        hub,
                        model.clone(),
                        progress,
                    ))
                    .await?;
                }
                this.update(cx, |this, cx| {
                    this.benchmark = Some(BenchStep::Measuring);
                    if !this.installed.contains(&model.id) {
                        this.installed.push(model.id.clone());
                    }
                    this.sync_local();
                    cx.notify();
                })
                .ok();
                ai::spawn(ai::bench::run(binary, path, dirs.logs())).await
            }
            .await;
            this.update(cx, |this, cx| {
                this.benchmark = None;
                match result {
                    Ok(speed) => {
                        this.measured = Some(speed);
                        this.verified.insert(model.id.clone(), speed);
                        this.save(cx);
                    }
                    Err(e) => this.error = Some(e.into()),
                }
                this.refresh_disk(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Installs every recommended model that is missing.
    pub fn install_recommended(&mut self, cx: &mut Context<Self>) {
        for c in self.candidates() {
            if !c.picks.is_empty() && !self.installed.contains(&c.model.id) {
                self.install(c.model, cx);
            }
        }
    }

    /// Measures an installed model again, on its own server.
    pub fn measure(&mut self, model: Model, cx: &mut Context<Self>) {
        let Some(hw) = self.hardware.clone() else {
            return;
        };
        if self.verifying.contains(&model.id) {
            return;
        }
        self.error = None;
        self.verifying.push(model.id.clone());
        self.tick(cx);
        let dirs = self.dirs.clone();
        cx.spawn(async move |this, cx| {
            let binary = Self::runtime(dirs.clone(), hw, Progress::new()).await;
            let path = dirs.model(&model);
            Self::verify(this, binary.map(|b| (b, path)), model, dirs, cx).await;
        })
        .detach();
    }

    /// Times `model` once nothing else is measuring, so two servers never
    /// compete for memory.
    async fn verify(
        this: gpui::WeakEntity<Self>,
        paths: Result<(PathBuf, PathBuf), String>,
        model: Model,
        dirs: Dirs,
        cx: &mut gpui::AsyncApp,
    ) {
        let speed = match paths {
            Ok((binary, path)) => {
                loop {
                    let free = this
                        .update(cx, |this, _| {
                            let free = this.benchmark.is_none()
                                && this.verifying.first() == Some(&model.id);
                            if free {
                                this.stop_local();
                            }
                            free
                        })
                        .unwrap_or(true);
                    if free {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                }
                ai::spawn(ai::bench::run(binary, path, dirs.logs())).await
            }
            Err(e) => Err(e),
        };
        this.update(cx, |this, cx| {
            this.verifying.retain(|id| *id != model.id);
            match speed {
                Ok(speed) => {
                    this.verified.insert(model.id.clone(), speed);
                    this.save(cx);
                }
                Err(e) => this.error = Some(format!("{}: {e}", model.name).into()),
            }
            cx.notify();
        })
        .ok();
    }

    /// Downloads `model`, then measures it, and gives it the roles it is
    /// recommended for that nothing else holds yet.
    pub fn install(&mut self, model: Model, cx: &mut Context<Self>) {
        let Some(hw) = self.hardware.clone() else {
            return;
        };
        if self.downloads.contains_key(&model.id) {
            return;
        }
        self.error = None;
        let progress = Progress::new();
        self.downloads.insert(model.id.clone(), progress.clone());
        self.tick(cx);
        let (dirs, hub) = (self.dirs.clone(), self.hub.clone());
        cx.spawn(async move |this, cx| {
            let result = async {
                let binary = Self::runtime(dirs.clone(), hw, Progress::new()).await?;
                let path = ai::spawn(install::install_model(
                    dirs.clone(),
                    hub,
                    model.clone(),
                    progress,
                ))
                .await?;
                Ok::<_, String>((binary, path))
            }
            .await;
            let installed = this.update(cx, |this, cx| {
                this.downloads.remove(&model.id);
                this.refresh_disk(cx);
                match result {
                    Ok(paths) => {
                        if !this.installed.contains(&model.id) {
                            this.installed.push(model.id.clone());
                        }
                        let picks = this
                            .candidates()
                            .into_iter()
                            .find(|c| c.model.id == model.id)
                            .map(|c| c.picks)
                            .unwrap_or_default();
                        for role in picks {
                            this.roles
                                .entry(role)
                                .or_insert_with(|| ModelRef::local(&model.id));
                        }
                        this.sync_local();
                        this.save(cx);
                        this.verifying.push(model.id.clone());
                        cx.notify();
                        Some(paths)
                    }
                    Err(e) => {
                        if e != "Cancelled" {
                            this.error = Some(format!("{}: {e}", model.name).into());
                        }
                        cx.notify();
                        None
                    }
                }
            });
            if let Ok(Some(paths)) = installed {
                Self::verify(this, Ok(paths), model, dirs, cx).await;
            }
        })
        .detach();
    }

    pub fn cancel(&mut self, model: &str, cx: &mut Context<Self>) {
        if let Some(progress) = self.downloads.get(model) {
            progress.cancel();
        }
        cx.notify();
    }

    /// Deletes a downloaded model. A model added by hand is also taken off
    /// the list; files Solder did not download stay on disk.
    pub fn remove(&mut self, model: Model, cx: &mut Context<Self>) {
        self.installed.retain(|id| *id != model.id);
        self.roles.retain(|_, m| *m != ModelRef::local(&model.id));
        self.stop_local_model(&model.id);
        self.sync_local();
        self.verified.remove(&model.id);
        self.custom.retain(|m| m.id != model.id);
        self.found.retain(|m| m.id != model.id);
        self.save(cx);
        let dirs = self.dirs.clone();
        let name = model.name.clone();
        let removed = cx
            .background_executor()
            .spawn(async move { install::remove_model(&dirs, &model) });
        cx.spawn(async move |this, cx| {
            let result = removed.await;
            this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.error = Some(format!("Could not delete {name}: {e}").into());
                }
                this.refresh_disk(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Adds a model from a Hugging Face repository or link, or a `.gguf`
    /// path, described by its file's header.
    pub fn add(&mut self, input: String, cx: &mut Context<Self>) {
        let input = input.trim().to_string();
        if input.is_empty() || self.adding {
            return;
        }
        self.adding = true;
        self.error = None;
        cx.notify();
        let lookup: Task<Result<Model, String>> = match local_path(&input) {
            Some(path) => cx
                .background_executor()
                .spawn(async move { custom::from_file(&path) }),
            None => {
                let hub = self.hub.clone();
                let future = ai::spawn(custom::from_hub(hub, input));
                cx.background_executor().spawn(future)
            }
        };
        cx.spawn(async move |this, cx| {
            let result = lookup.await;
            this.update(cx, |this, cx| {
                this.adding = false;
                match result {
                    Ok(model) => {
                        if this.models().iter().any(|m| m.id == model.id) {
                            this.error =
                                Some(format!("{} is already in the list", model.name).into());
                        } else {
                            if matches!(&model.source, Source::File(p) if p.is_file()) {
                                this.installed.push(model.id.clone());
                            }
                            this.custom.push(model);
                            this.sync_local();
                            this.save(cx);
                        }
                    }
                    Err(e) => this.error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Gives a local model a task, or takes it back.
    pub fn set_role(&mut self, role: Role, model: &str, cx: &mut Context<Self>) {
        let model = ModelRef::local(model);
        if self.roles.get(&role) == Some(&model) {
            self.roles.remove(&role);
        } else {
            self.roles.insert(role, model);
        }
        self.save(cx);
        cx.notify();
    }

    pub fn is_found(&self, id: &str) -> bool {
        self.found.iter().any(|m| m.id == id)
    }

    fn refresh_disk(&mut self, cx: &mut Context<Self>) {
        let root = self.dirs.root.clone();
        let free = cx
            .background_executor()
            .spawn(async move { ai::free_space(&root) });
        cx.spawn(async move |this, cx| {
            let free = free.await;
            this.update(cx, |this, cx| {
                this.free_disk = free;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_paths_from_hub_names() {
        assert_eq!(local_path("/m/x.gguf"), Some(PathBuf::from("/m/x.gguf")));
        assert!(local_path("~/x.gguf").is_some_and(|p| p.is_absolute()));
        assert_eq!(local_path("org/repo"), None);
        assert_eq!(local_path("org/repo/file.gguf"), None);
    }
}
