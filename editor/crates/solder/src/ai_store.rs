//! Local models in the app: what this machine can run, the benchmark,
//! downloads and which model serves each role. One store for all windows,
//! since models are shared and only one server should hold memory.
//!
//! Nothing runs until the AI tab is opened.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use ai::{Candidate, Dirs, Hardware, Model, Progress, Role, Speed, catalog, install};
use gpui::{App, AppContext, Context, Entity, Global, SharedString, Task};

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
    pub loaded: bool,
    pub hardware: Option<Hardware>,
    pub free_disk: Option<u64>,
    /// The calibration model's speed on this machine.
    pub measured: Option<Speed>,
    /// Speeds measured on each installed model after its download.
    pub verified: HashMap<String, Speed>,
    pub roles: HashMap<Role, String>,
    pub installed: Vec<&'static str>,
    pub benchmark: Option<BenchStep>,
    pub downloads: HashMap<&'static str, Arc<Progress>>,
    /// Models being measured after their download.
    pub verifying: Vec<&'static str>,
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

impl AiStore {
    pub fn new(dirs: Dirs, hub: String) -> Self {
        Self {
            dirs,
            hub,
            loaded: false,
            hardware: None,
            free_disk: None,
            measured: None,
            verified: HashMap::new(),
            roles: HashMap::new(),
            installed: Vec::new(),
            benchmark: None,
            downloads: HashMap::new(),
            verifying: Vec::new(),
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
        cx.set_global(GlobalAiStore(store.clone()));
        store
    }

    #[cfg(test)]
    pub fn set_global(store: Entity<AiStore>, cx: &mut App) {
        cx.set_global(GlobalAiStore(store));
    }

    /// Reads the machine and what is installed, the first time.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let dirs = self.dirs.clone();
        let read = cx.background_executor().spawn(async move {
            let hw = ai::hardware::detect();
            let free = ai::free_space(&dirs.root);
            let state = std::fs::read(dirs.state())
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
            let installed = install::installed_models(&dirs);
            (hw, free, state, installed)
        });
        cx.spawn(async move |this, cx| {
            let (hw, free, state, installed) = read.await;
            this.update(cx, |this, cx| {
                this.hardware = Some(hw);
                this.free_disk = free;
                this.installed = installed.iter().map(|m| m.id).collect();
                if let Some(state) = state {
                    this.measured = Speed::from_json(&state["calibration"]);
                    if let Some(map) = state["verified"].as_object() {
                        for (id, speed) in map {
                            if let Some(s) = Speed::from_json(speed) {
                                this.verified.insert(id.clone(), s);
                            }
                        }
                    }
                    for role in [Role::Chat, Role::Completion] {
                        if let Some(id) = state["roles"][role_key(role)].as_str()
                            && this.installed.contains(&id)
                        {
                            this.roles.insert(role, id.to_string());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save(&self, cx: &mut Context<Self>) {
        let mut roles = serde_json::Map::new();
        for (role, id) in &self.roles {
            roles.insert(role_key(*role).into(), id.clone().into());
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
            Some(hw) => catalog::recommend(hw, self.measured, &self.verified),
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
        self.benchmark = Some(BenchStep::Runtime(progress.clone()));
        self.tick(cx);
        cx.spawn(async move |this, cx| {
            let result: Result<Speed, String> = async {
                let binary = Self::runtime(dirs.clone(), hw, progress).await?;
                let model = catalog::calibration();
                let path = dirs.model(model);
                if !path.is_file() {
                    let progress = Progress::new();
                    this.update(cx, |this, _| {
                        this.benchmark = Some(BenchStep::Model(progress.clone()))
                    })
                    .ok();
                    ai::spawn(install::install_model(dirs.clone(), hub, model, progress)).await?;
                }
                this.update(cx, |this, cx| {
                    this.benchmark = Some(BenchStep::Measuring);
                    if !this.installed.contains(&model.id) {
                        this.installed.push(model.id);
                    }
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
                        this.verified
                            .insert(catalog::calibration().id.to_string(), speed);
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

    /// Downloads `model`, then measures it, and gives it the roles it is
    /// recommended for that nothing else holds yet.
    pub fn install(&mut self, model: &'static Model, cx: &mut Context<Self>) {
        let Some(hw) = self.hardware.clone() else {
            return;
        };
        if self.downloads.contains_key(model.id) {
            return;
        }
        self.error = None;
        let progress = Progress::new();
        self.downloads.insert(model.id, progress.clone());
        self.tick(cx);
        let (dirs, hub) = (self.dirs.clone(), self.hub.clone());
        cx.spawn(async move |this, cx| {
            let result = async {
                let binary = Self::runtime(dirs.clone(), hw, Progress::new()).await?;
                let path =
                    ai::spawn(install::install_model(dirs.clone(), hub, model, progress)).await?;
                Ok::<_, String>((binary, path))
            }
            .await;
            let installed = this.update(cx, |this, cx| {
                this.downloads.remove(model.id);
                this.refresh_disk(cx);
                match result {
                    Ok(paths) => {
                        if !this.installed.contains(&model.id) {
                            this.installed.push(model.id);
                        }
                        let picks = this
                            .candidates()
                            .into_iter()
                            .find(|c| c.model.id == model.id)
                            .map(|c| c.picks)
                            .unwrap_or_default();
                        for role in picks {
                            this.roles.entry(role).or_insert_with(|| model.id.into());
                        }
                        this.save(cx);
                        this.verifying.push(model.id);
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
            let Ok(Some((binary, path))) = installed else {
                return;
            };
            // Wait for the benchmark or another check to free the memory.
            loop {
                let free = this
                    .update(cx, |this, _| {
                        this.benchmark.is_none() && this.verifying.first() == Some(&model.id)
                    })
                    .unwrap_or(true);
                if free {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            }
            let logs = dirs.logs();
            let speed = ai::spawn(ai::bench::run(binary, path, logs)).await;
            this.update(cx, |this, cx| {
                this.verifying.retain(|id| *id != model.id);
                match speed {
                    Ok(speed) => {
                        this.verified.insert(model.id.into(), speed);
                        this.save(cx);
                    }
                    Err(e) => this.error = Some(format!("{}: {e}", model.name).into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn cancel(&mut self, model: &str, cx: &mut Context<Self>) {
        if let Some(progress) = self.downloads.get(model) {
            progress.cancel();
        }
        cx.notify();
    }

    pub fn remove(&mut self, model: &'static Model, cx: &mut Context<Self>) {
        self.installed.retain(|id| *id != model.id);
        self.roles.retain(|_, id| id != model.id);
        self.verified.remove(model.id);
        self.save(cx);
        let dirs = self.dirs.clone();
        let removed = cx
            .background_executor()
            .spawn(async move { install::remove_model(&dirs, model) });
        cx.spawn(async move |this, cx| {
            let result = removed.await;
            this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.error = Some(format!("Could not delete {}: {e}", model.name).into());
                }
                this.refresh_disk(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn set_role(&mut self, role: Role, model: &str, cx: &mut Context<Self>) {
        if self.roles.get(&role).map(String::as_str) == Some(model) {
            self.roles.remove(&role);
        } else {
            self.roles.insert(role, model.into());
        }
        self.save(cx);
        cx.notify();
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
