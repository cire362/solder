//! Where chat answers come from: the local llama.cpp server, Ollama and LM
//! Studio when they run, Anthropic and OpenAI with a key, and any
//! OpenAI-compatible service added by hand. Each task (chat, completions)
//! picks a model from any of them; offline mode keeps to this machine.

use std::time::{Duration, Instant};

use ai::provider::{Api, Endpoint};
use gpui::{Context, SharedString, Task};

use crate::ai_store::AiStore;

/// A model at a provider.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl ModelRef {
    pub fn local(id: &str) -> Self {
        Self {
            provider: LOCAL.into(),
            model: id.into(),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "provider": self.provider, "model": self.model })
    }

    /// Also reads the bare local model ids saved before providers existed.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        if let Some(id) = value.as_str() {
            return Some(Self::local(id));
        }
        Some(Self {
            provider: value["provider"].as_str()?.into(),
            model: value["model"].as_str()?.into(),
        })
    }
}

pub const LOCAL: &str = "local";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Local,
    Ollama,
    LmStudio,
    Anthropic,
    OpenAi,
    Compatible,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Unknown,
    Checking,
    Ready(Vec<String>),
    NoKey,
    Unavailable(SharedString),
    /// A network provider while offline mode is on.
    Offline,
}

#[derive(Clone, Debug)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub base_url: String,
    pub status: Status,
    /// `saved` or the environment variable the key comes from.
    pub key_source: Option<&'static str>,
}

impl ProviderInfo {
    pub fn api(&self) -> Api {
        match self.kind {
            Kind::Anthropic => Api::Anthropic,
            _ => Api::OpenAi,
        }
    }

    pub fn needs_key(&self) -> bool {
        matches!(self.kind, Kind::Anthropic | Kind::OpenAi)
    }

    pub fn takes_key(&self) -> bool {
        matches!(self.kind, Kind::Anthropic | Kind::OpenAi | Kind::Compatible)
    }

    /// Off this machine: what offline mode turns off.
    pub fn is_remote(&self) -> bool {
        if self.kind == Kind::Local {
            return false;
        }
        let host = self
            .base_url
            .split("://")
            .nth(1)
            .unwrap_or(&self.base_url)
            .split(['/', ':'])
            .next()
            .unwrap_or("");
        !matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
    }

    pub fn models(&self) -> &[String] {
        match &self.status {
            Status::Ready(models) => models,
            _ => &[],
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "id": self.id, "name": self.name, "url": self.base_url })
    }

    pub fn compatible_from_json(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            id: value["id"].as_str()?.into(),
            name: value["name"].as_str()?.into(),
            kind: Kind::Compatible,
            base_url: value["url"].as_str()?.into(),
            status: Status::Unknown,
            key_source: None,
        })
    }
}

/// The addresses of the built-in providers; tests point them elsewhere.
#[derive(Clone, Debug)]
pub struct Urls {
    pub ollama: String,
    pub lm_studio: String,
    pub anthropic: String,
    pub openai: String,
}

impl Default for Urls {
    fn default() -> Self {
        Self {
            ollama: "http://localhost:11434/v1".into(),
            lm_studio: "http://localhost:1234/v1".into(),
            anthropic: "https://api.anthropic.com/v1".into(),
            openai: "https://api.openai.com/v1".into(),
        }
    }
}

pub fn builtin(urls: &Urls) -> Vec<ProviderInfo> {
    let p = |id: &str, name: &str, kind: Kind, url: &str| ProviderInfo {
        id: id.into(),
        name: name.into(),
        kind,
        base_url: url.into(),
        status: Status::Unknown,
        key_source: None,
    };
    vec![
        p(LOCAL, "This machine", Kind::Local, ""),
        p("ollama", "Ollama", Kind::Ollama, &urls.ollama),
        p("lmstudio", "LM Studio", Kind::LmStudio, &urls.lm_studio),
        p("anthropic", "Anthropic", Kind::Anthropic, &urls.anthropic),
        p("openai", "OpenAI", Kind::OpenAi, &urls.openai),
    ]
}

/// A provider id from a name: `My Proxy` becomes `custom-my-proxy`.
fn custom_id(name: &str) -> String {
    let slug: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("custom-{}", slug.trim_matches('-'))
}

/// A running local server and when it last answered.
pub struct Running {
    pub model: String,
    pub server: ai::LocalServer,
    pub used: Instant,
}

/// A local server unused this long is stopped, to give its memory back.
const IDLE: Duration = Duration::from_secs(10 * 60);
/// Local servers at once: the chat model and the completion model.
const MAX_LOCAL: usize = 2;

impl AiStore {
    pub fn provider(&self, id: &str) -> Option<&ProviderInfo> {
        self.providers.iter().find(|p| p.id == id)
    }

    fn provider_mut(&mut self, id: &str) -> Option<&mut ProviderInfo> {
        self.providers.iter_mut().find(|p| p.id == id)
    }

    /// Checks every provider: which run, which have keys, their models.
    pub fn refresh_providers(&mut self, cx: &mut Context<Self>) {
        let offline = self.offline;
        let installed = self.installed.clone();
        let keys = self.keys.clone();
        for p in &mut self.providers {
            p.status = if p.kind == Kind::Local {
                Status::Ready(installed.clone())
            } else if offline && p.is_remote() {
                Status::Offline
            } else {
                Status::Checking
            };
        }
        let checks: Vec<ProviderInfo> = self
            .providers
            .iter()
            .filter(|p| p.status == Status::Checking)
            .cloned()
            .collect();
        for p in checks {
            let keys = keys.clone();
            let lookup = cx.background_executor().spawn({
                let id = p.id.clone();
                async move { (keys.get(&id), keys.source(&id)) }
            });
            cx.spawn(async move |this, cx| {
                let (key, source) = lookup.await;
                let status = if p.needs_key() && key.is_none() {
                    Status::NoKey
                } else {
                    let endpoint = Endpoint {
                        api: p.api(),
                        base_url: p.base_url.clone(),
                        key,
                    };
                    match ai::spawn(ai::provider::list_models(endpoint)).await {
                        Ok(models) if models.is_empty() => {
                            Status::Unavailable("No models loaded".into())
                        }
                        Ok(models) => Status::Ready(models),
                        Err(e) => Status::Unavailable(e.into()),
                    }
                };
                this.update(cx, |this, cx| {
                    if let Some(info) = this.provider_mut(&p.id) {
                        info.status = status;
                        info.key_source = source;
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    pub fn set_key(&mut self, id: &str, key: String, cx: &mut Context<Self>) {
        let keys = self.keys.clone();
        let account = id.to_string();
        let saved = cx
            .background_executor()
            .spawn(async move { keys.set(&account, &key) });
        cx.spawn(async move |this, cx| {
            let result = saved.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.refresh_providers(cx),
                    Err(e) => this.error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn delete_key(&mut self, id: &str, cx: &mut Context<Self>) {
        let keys = self.keys.clone();
        let account = id.to_string();
        let deleted = cx
            .background_executor()
            .spawn(async move { keys.delete(&account) });
        cx.spawn(async move |this, cx| {
            deleted.await.ok();
            this.update(cx, |this, cx| this.refresh_providers(cx)).ok();
        })
        .detach();
    }

    /// Adds an OpenAI-compatible service: a name and its address up to the
    /// version (`https://host/v1`).
    pub fn add_provider(
        &mut self,
        name: String,
        url: String,
        key: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let name = name.trim().to_string();
        let url = url.trim().trim_end_matches('/').to_string();
        if name.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            self.error = Some("Give the provider a name and an http(s) address".into());
            cx.notify();
            return;
        }
        let id = custom_id(&name);
        if self.provider(&id).is_some() {
            self.error = Some(format!("{name} is already a provider").into());
            cx.notify();
            return;
        }
        self.providers.push(ProviderInfo {
            id: id.clone(),
            name,
            kind: Kind::Compatible,
            base_url: url,
            status: Status::Unknown,
            key_source: None,
        });
        self.save(cx);
        match key.filter(|k| !k.trim().is_empty()) {
            Some(key) => self.set_key(&id, key, cx),
            None => self.refresh_providers(cx),
        }
    }

    pub fn remove_provider(&mut self, id: &str, cx: &mut Context<Self>) {
        if self
            .provider(id)
            .is_some_and(|p| p.kind == Kind::Compatible)
        {
            self.providers.retain(|p| p.id != id);
            self.roles.retain(|_, m| m.provider != id);
            self.delete_key(id, cx);
            self.save(cx);
        }
    }

    pub fn set_offline(&mut self, offline: bool, cx: &mut Context<Self>) {
        self.offline = offline;
        self.save(cx);
        self.refresh_providers(cx);
    }

    pub fn choose(&mut self, role: ai::Role, model: ModelRef, cx: &mut Context<Self>) {
        self.roles.insert(role, model);
        self.save(cx);
        cx.notify();
    }

    /// What to show for a model: a catalog name for local models.
    pub fn model_label(&self, model: &ModelRef) -> String {
        if model.provider == LOCAL {
            return self
                .models()
                .into_iter()
                .find(|m| m.id == model.model)
                .map_or(model.model.clone(), |m| m.name);
        }
        model.model.clone()
    }

    /// Every model that can answer now, by provider.
    pub fn choices(&self) -> Vec<(ProviderInfo, Vec<(ModelRef, String)>)> {
        self.providers
            .iter()
            .filter_map(|p| {
                let models: Vec<(ModelRef, String)> = p
                    .models()
                    .iter()
                    .map(|m| {
                        let r = ModelRef {
                            provider: p.id.clone(),
                            model: m.clone(),
                        };
                        let label = self.model_label(&r);
                        (r, label)
                    })
                    .collect();
                (!models.is_empty()).then(|| (p.clone(), models))
            })
            .collect()
    }

    /// Where to send a request for `model`: a key from the store, or the
    /// local server, started for this model if it is not running.
    pub fn endpoint(
        &mut self,
        model: &ModelRef,
        cx: &mut Context<Self>,
    ) -> Task<Result<Endpoint, String>> {
        let Some(provider) = self.provider(&model.provider).cloned() else {
            return Task::ready(Err(format!("No provider {}", model.provider)));
        };
        if self.offline && provider.is_remote() {
            return Task::ready(Err(format!("{} is off in offline mode", provider.name)));
        }
        if provider.kind == Kind::Local {
            return self.local_endpoint(model.model.clone(), cx);
        }
        let keys = self.keys.clone();
        let id = provider.id.clone();
        cx.background_executor().spawn(async move {
            let key = keys.get(&id);
            if provider.needs_key() && key.is_none() {
                return Err(format!("Add a key for {} in the AI tab", provider.name));
            }
            Ok(Endpoint {
                api: provider.api(),
                base_url: provider.base_url,
                key,
            })
        })
    }

    fn local_endpoint(
        &mut self,
        id: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<Endpoint, String>> {
        self.local.retain_mut(|r| r.server.is_running());
        if let Some(running) = self.local.iter_mut().find(|r| r.model == id) {
            running.used = Instant::now();
            return Task::ready(Ok(Self::local_api(&running.server)));
        }
        if self.starting.as_deref() == Some(id.as_str()) {
            // Chat and completions may ask at once: wait for the one start.
            return cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    let ready = this
                        .update(cx, |this, _| {
                            if this.starting.as_deref() == Some(id.as_str()) {
                                return None;
                            }
                            Some(
                                this.local
                                    .iter()
                                    .find(|r| r.model == id)
                                    .map(|r| Self::local_api(&r.server))
                                    .ok_or_else(|| "The model did not start".to_string()),
                            )
                        })
                        .map_err(|e| e.to_string())?;
                    if let Some(ready) = ready {
                        return ready;
                    }
                }
            });
        }
        let Some(model) = self.models().into_iter().find(|m| m.id == id) else {
            return Task::ready(Err("That model is not installed".into()));
        };
        let Some(binary) = ai::install::installed_runtime(&self.dirs) else {
            return Task::ready(Err(
                "Run the benchmark in the AI tab to install llama.cpp".into()
            ));
        };
        // Room for the chat model and the completion model; a third
        // replaces the one used longest ago.
        while self.local.len() >= MAX_LOCAL {
            let oldest = (0..self.local.len())
                .min_by_key(|&i| self.local[i].used)
                .unwrap_or(0);
            self.local.remove(oldest).server.stop();
        }
        self.starting = Some(id.clone());
        cx.notify();
        let path = self.dirs.model(&model);
        let logs = self.dirs.logs().join(model.id.replace(['/', ':'], "_"));
        let context = model.context;
        cx.spawn(async move |this, cx| {
            let started = ai::spawn(ai::LocalServer::start(binary, path, context, logs)).await;
            this.update(cx, |this, cx| {
                this.starting = None;
                cx.notify();
                let server = started?;
                let endpoint = Self::local_api(&server);
                this.local.push(Running {
                    model: id,
                    server,
                    used: Instant::now(),
                });
                if this.local.len() == 1 {
                    this.watch_idle(cx);
                }
                Ok(endpoint)
            })
            .map_err(|e| e.to_string())?
        })
    }

    fn local_api(server: &ai::LocalServer) -> Endpoint {
        Endpoint {
            api: Api::OpenAi,
            base_url: format!("{}/v1", server.url()),
            key: Some(server.key.clone()),
        }
    }

    /// Marks a local model's server as used, so it is not stopped while in
    /// use.
    pub fn touch(&mut self, model: &ModelRef) {
        if model.provider != LOCAL {
            return;
        }
        if let Some(running) = self.local.iter_mut().find(|r| r.model == model.model) {
            running.used = Instant::now();
        }
    }

    /// Marks the model serving `role` as used.
    pub fn touch_role(&mut self, role: ai::Role) {
        if let Some(model) = self.roles.get(&role).cloned() {
            self.touch(&model);
        }
    }

    /// Stops every local server: before measuring, and on quit.
    pub fn stop_local(&mut self) {
        for mut running in self.local.drain(..) {
            running.server.stop();
        }
    }

    pub fn stop_local_model(&mut self, id: &str) {
        if let Some(i) = self.local.iter().position(|r| r.model == id) {
            self.local.remove(i).server.stop();
        }
    }

    #[cfg(test)]
    pub fn is_running(&self, id: &str) -> bool {
        self.local.iter().any(|r| r.model == id)
    }

    fn watch_idle(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
                let running = this
                    .update(cx, |this, cx| {
                        let before = this.local.len();
                        this.local.retain_mut(|r| {
                            let idle = r.used.elapsed() > IDLE;
                            if idle {
                                r.server.stop();
                            }
                            !idle
                        });
                        if this.local.len() != before {
                            cx.notify();
                        }
                        !this.local.is_empty()
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_local_from_remote_and_reads_old_roles() {
        let urls = Urls::default();
        let all = builtin(&urls);
        let remote: Vec<_> = all
            .iter()
            .filter(|p| p.is_remote())
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(remote, ["anthropic", "openai"]);
        assert_eq!(
            ModelRef::from_json(&serde_json::json!("qwen3.5-9b")),
            Some(ModelRef::local("qwen3.5-9b"))
        );
        let r = ModelRef {
            provider: "anthropic".into(),
            model: "claude".into(),
        };
        assert_eq!(ModelRef::from_json(&r.to_json()), Some(r));
        assert_eq!(custom_id("My Proxy!"), "custom-my-proxy");
    }
}
