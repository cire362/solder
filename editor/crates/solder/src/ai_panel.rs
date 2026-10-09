//! The AI tab: this machine, the benchmark, and the models it can run, with
//! install, delete, models added by hand and which model serves chat and
//! completions.

use ai::{Candidate, Role, Source, Speed, format_size};
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, Hsla, KeyBinding, PathPromptOptions,
    SharedString, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::{
    ai_providers::{Kind, Status},
    ai_store::{AiStore, BenchStep},
    editor::Editor,
    mcp_store::{McpStore, State},
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

const ROW_HEIGHT: gpui::Pixels = px(54.);

actions!(ai_panel, [AddModel, SaveKey, AddProvider]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", AddModel, Some("AiAddModel")),
        KeyBinding::new("enter", SaveKey, Some("AiKey")),
        KeyBinding::new("enter", AddProvider, Some("AiNewProvider")),
    ]);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Models,
    Providers,
    /// The context servers the agent may use.
    Servers,
}

pub struct AiPanel {
    store: Entity<AiStore>,
    pub view: View,
    add_field: Entity<Editor>,
    /// The provider whose key is being typed.
    key_for: Option<String>,
    key_field: Entity<Editor>,
    new_provider: bool,
    completion_picker: bool,
    provider_name: Entity<Editor>,
    provider_url: Entity<Editor>,
    provider_key: Entity<Editor>,
    /// Hears the context servers start and stop, once their list is shown.
    servers_watch: Option<gpui::Subscription>,
    focus: FocusHandle,
}

impl AiPanel {
    pub fn new(store: Entity<AiStore>, cx: &mut Context<Self>) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        let add_field = cx.new(|cx| Editor::single_line("org/model-GGUF or a .gguf path", cx));
        Self {
            store,
            view: View::Models,
            add_field,
            key_for: None,
            key_field: cx.new(|cx| Editor::masked("Paste the API key", cx)),
            new_provider: false,
            completion_picker: false,
            provider_name: cx.new(|cx| Editor::single_line("Name", cx)),
            provider_url: cx.new(|cx| Editor::single_line("https://host/v1", cx)),
            provider_key: cx.new(|cx| Editor::masked("API key, if it needs one", cx)),
            servers_watch: None,
            focus: cx.focus_handle(),
        }
    }

    pub fn show(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        if view == View::Providers {
            self.store.update(cx, |s, cx| s.refresh_providers(cx));
        }
        if view == View::Servers && self.servers_watch.is_none() {
            let servers = McpStore::global(cx);
            self.servers_watch = Some(cx.observe(&servers, |_, _, cx| cx.notify()));
        }
        cx.notify();
    }

    fn save_key(&mut self, _: &SaveKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.key_for.take() else {
            return;
        };
        // The field goes away; keep keys working.
        window.focus(&self.focus);
        let key = self.key_field.read(cx).text(cx);
        self.key_field.update(cx, |e, cx| e.set_text("", false, cx));
        if !key.trim().is_empty() {
            self.store.update(cx, |s, cx| s.set_key(&id, key, cx));
        }
        cx.notify();
    }

    fn add_provider(&mut self, _: &AddProvider, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let name = self.provider_name.read(cx).text(cx);
        let url = self.provider_url.read(cx).text(cx);
        let key = self.provider_key.read(cx).text(cx);
        for field in [&self.provider_name, &self.provider_url, &self.provider_key] {
            field.update(cx, |e, cx| e.set_text("", false, cx));
        }
        self.new_provider = false;
        self.store
            .update(cx, |s, cx| s.add_provider(name, url, Some(key), cx));
        cx.notify();
    }

    fn render_providers(
        &self,
        window: &Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let store = self.store.read(cx);
        let small = crate::theme::UI_FONT_SMALL;
        let entity = self.store.clone();
        let panel = cx.entity();
        let mut list = div().flex().flex_col().gap_0p5().px_1p5();
        for p in store.providers.iter().filter(|p| p.kind != Kind::Local) {
            let status: (String, Hsla) = match &p.status {
                Status::Unknown | Status::Checking => ("Checking...".into(), theme.fg_subtle),
                Status::Ready(models) => (
                    match models.len() {
                        1 => "1 model".into(),
                        n => format!("{n} models"),
                    },
                    theme.git_added,
                ),
                Status::NoKey => ("No key".into(), theme.fg_subtle),
                Status::Unavailable(e) => (e.to_string(), theme.fg_subtle),
                Status::Offline => ("Off in offline mode".into(), theme.fg_subtle),
            };
            let key_note = p.key_source.map(|source| match source {
                "saved" => "key saved".to_string(),
                var => format!("key from {var}"),
            });
            let id = p.id.clone();
            let editing = self.key_for.as_deref() == Some(id.as_str());
            let mut actions = div().flex().items_center().gap_0p5();
            if p.takes_key()
                && p.key_source != Some("ANTHROPIC_API_KEY")
                && p.key_source != Some("OPENAI_API_KEY")
            {
                let (label, sel) = if p.key_source.is_some() {
                    ("forget key", format!("ai-forget-key-{id}"))
                } else {
                    ("add key", format!("ai-key-{id}"))
                };
                let forget = p.key_source.is_some();
                let entity = entity.clone();
                let panel = panel.clone();
                let target = id.clone();
                let selector = sel.clone();
                actions = actions.child(
                    ui::toggle(
                        SharedString::from(sel),
                        label,
                        "",
                        editing,
                        theme,
                        move |_, window, cx| {
                            if forget {
                                entity.update(cx, |s, cx| s.delete_key(&target, cx));
                            } else {
                                panel.update(cx, |this, cx| {
                                    this.key_for = Some(target.clone());
                                    cx.notify();
                                });
                                let field = panel.read(cx).key_field.clone();
                                window.focus(&field.focus_handle(cx));
                            }
                        },
                    )
                    .debug_selector(move || selector.clone()),
                );
            }
            if p.kind == Kind::Compatible {
                let entity = entity.clone();
                let target = id.clone();
                actions = actions.child(ui::toggle(
                    SharedString::from(format!("ai-remove-provider-{id}")),
                    "remove",
                    "",
                    false,
                    theme,
                    move |_, _, cx| entity.update(cx, |s, cx| s.remove_provider(&target, cx)),
                ));
            }
            list = list.child(
                div()
                    .id(SharedString::from(format!("ai-provider-{id}")))
                    .debug_selector({
                        let id = id.clone();
                        move || format!("ai-provider-{id}")
                    })
                    .px_2()
                    .py_1p5()
                    .rounded(theme.shape.control)
                    .hover(|d| d.bg(theme.bg_elev))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(UI_FONT_SIZE)
                                    .text_color(theme.fg)
                                    .child(p.name.clone()),
                            )
                            .child(actions),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::theme::text(11.))
                            .text_color(status.1)
                            .child(match key_note {
                                Some(note) => format!("{} · {note}", status.0),
                                None => status.0,
                            }),
                    )
                    .when(editing, |d| {
                        let focused = self.key_field.focus_handle(cx).is_focused(window);
                        d.child(
                            div()
                                .pt_1()
                                .flex()
                                .gap_1p5()
                                .key_context("AiKey")
                                .on_action(cx.listener(Self::save_key))
                                .child(ui::text_field(self.key_field.clone(), focused, theme))
                                .child(
                                    ui::button(
                                        "ai-save-key",
                                        "Save",
                                        true,
                                        theme,
                                        cx.listener(|this, _, window, cx| {
                                            this.save_key(&SaveKey, window, cx)
                                        }),
                                    )
                                    .debug_selector(|| "ai-save-key".into()),
                                ),
                        )
                    }),
            );
        }
        let form = if self.new_provider {
            let field = |editor: &Entity<Editor>| {
                let focused = editor.focus_handle(cx).is_focused(window);
                ui::text_field(editor.clone(), focused, theme)
            };
            div()
                .px_3()
                .pt_2()
                .flex()
                .flex_col()
                .gap_1p5()
                .key_context("AiNewProvider")
                .on_action(cx.listener(Self::add_provider))
                .child(
                    div()
                        .text_size(small)
                        .text_color(theme.fg_subtle)
                        .child("Any service with OpenAI's chat completions API."),
                )
                .child(div().flex().child(field(&self.provider_name)))
                .child(div().flex().child(field(&self.provider_url)))
                .child(div().flex().child(field(&self.provider_key)))
                .child(
                    div()
                        .flex()
                        .gap_1p5()
                        .child(
                            ui::button(
                                "ai-add-provider",
                                "Add provider",
                                true,
                                theme,
                                cx.listener(|this, _, window, cx| {
                                    this.add_provider(&AddProvider, window, cx)
                                }),
                            )
                            .debug_selector(|| "ai-add-provider".into()),
                        )
                        .child(ui::button(
                            "ai-cancel-provider",
                            "Cancel",
                            false,
                            theme,
                            cx.listener(|this, _, _, cx| {
                                this.new_provider = false;
                                cx.notify();
                            }),
                        )),
                )
                .into_any_element()
        } else {
            div()
                .px_3()
                .pt_2()
                .flex()
                .gap_1p5()
                .child(
                    ui::button(
                        "ai-new-provider",
                        "Add provider",
                        false,
                        theme,
                        cx.listener(|this, _, window, cx| {
                            this.new_provider = true;
                            window.focus(&this.provider_name.focus_handle(cx));
                            cx.notify();
                        }),
                    )
                    .debug_selector(|| "ai-new-provider".into()),
                )
                .child(ui::button(
                    "ai-refresh-providers",
                    "Check again",
                    false,
                    theme,
                    {
                        let entity = entity.clone();
                        move |_, _, cx| entity.update(cx, |s, cx| s.refresh_providers(cx))
                    },
                ))
                .into_any_element()
        };
        let completions = self.render_completions(theme, cx);
        let store = self.store.read(cx);
        let entity = self.store.clone();
        let offline = store.offline;
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_size(small)
                            .text_color(theme.fg_subtle)
                            .child(if offline {
                                "Offline: only this machine answers."
                            } else {
                                "Local servers, and services with your key."
                            }),
                    )
                    .child(
                        ui::toggle(
                            "ai-offline",
                            "offline",
                            "",
                            offline,
                            theme,
                            move |_, _, cx| entity.update(cx, |s, cx| s.set_offline(!offline, cx)),
                        )
                        .debug_selector(|| "ai-offline".into()),
                    ),
            )
            .child(list)
            .child(form)
            .child(completions)
            .child(self.render_review_toggle(theme, cx))
            .into_any_element()
    }

    /// The context servers of the settings and how each is doing. They
    /// start with an agent task, so before the first one all of them wait.
    fn render_servers(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let small = crate::theme::UI_FONT_SMALL;
        let configured = McpStore::listed(cx);
        let store = McpStore::global(cx);
        let store = store.read(cx);
        let mut list = div().px_3().flex().flex_col().gap_2();
        for (i, (name, config, extension)) in configured.iter().enumerate() {
            let entry = store.servers.iter().find(|entry| entry.name == *name);
            let (state, color, tools) = match entry.map(|entry| &entry.state) {
                _ if !config.enabled => ("Off".to_string(), theme.fg_subtle, String::new()),
                Some(State::Running(_, tools)) => (
                    match tools.len() {
                        1 => "1 tool".to_string(),
                        n => format!("{n} tools"),
                    },
                    theme.fg_muted,
                    tools
                        .iter()
                        .map(|tool| tool.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                Some(State::Starting) => ("Starting...".to_string(), theme.fg_muted, String::new()),
                Some(State::Failed(why)) => (why.to_string(), theme.error, String::new()),
                Some(State::Stopped) | None => (
                    "Starts with the next agent task".to_string(),
                    theme.fg_subtle,
                    String::new(),
                ),
            };
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .debug_selector(move || format!("context-server-{i}"))
                    .child(div().text_size(UI_FONT_SIZE).text_color(theme.fg).child(
                        match extension {
                            Some(extension) => format!("{name} (extension {extension})"),
                            None => name.clone(),
                        },
                    ))
                    .child(div().text_size(small).text_color(color).child(state))
                    .when(!tools.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(small)
                                .text_color(theme.fg_subtle)
                                .child(tools),
                        )
                    }),
            );
        }
        let hint = if configured.is_empty() {
            "A context server gives the agent tools of its own: a database to query, an issue tracker to read. Add one under context_servers in settings.json, with the command that starts it, or install an extension that brings one."
        } else {
            "They start when an agent task begins. A tool runs outside the agent's sandbox, so the agent asks before the first call of each."
        };
        list.child(
            div()
                .text_size(small)
                .text_color(theme.fg_subtle)
                .child(hint),
        )
        .into_any_element()
    }

    fn render_review_toggle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let on = self.store.read(cx).review_push;
        let entity = self.store.clone();
        div()
            .px_3()
            .pt_4()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .text_size(crate::theme::text(11.))
                    .text_color(theme.fg_subtle)
                    .child("REVIEW BEFORE PUSH"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_size(crate::theme::UI_FONT_SMALL)
                            .text_color(theme.fg_subtle)
                            .child("Push in the Git tab first has the chat model read what it sends; problems stop the push until you decide."),
                    )
                    .child(
                        ui::toggle("ai-review-push", "on", "", on, theme, move |_, _, cx| {
                            entity.update(cx, |s, cx| s.set_review_push(!on, cx))
                        })
                        .debug_selector(|| "ai-review-push".into()),
                    ),
            )
            .into_any_element()
    }

    /// Which model suggests code while typing, and whether it does.
    fn render_completions(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let small = crate::theme::UI_FONT_SMALL;
        let current = store.roles.get(&Role::Completion).cloned();
        let label = current
            .as_ref()
            .map_or("No model".to_string(), |m| store.model_label(m));
        let on = store.completions;
        let entity = self.store.clone();
        let mut block = div()
            .px_3()
            .pt_4()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .text_size(crate::theme::text(11.))
                    .text_color(theme.fg_subtle)
                    .child("COMPLETIONS"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("ai-completion-model")
                            .debug_selector(|| "ai-completion-model".into())
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_1()
                            .rounded(theme.shape.control)
                            .border(theme.shape.border)
                            .border_color(theme.line)
                            .hover(|d| d.bg(theme.bg_elev))
                            .truncate()
                            .text_size(UI_FONT_SIZE)
                            .text_color(if current.is_some() { theme.fg } else { theme.fg_subtle })
                            .child(label)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.completion_picker = !this.completion_picker;
                                cx.notify();
                            })),
                    )
                    .child(
                        ui::toggle("ai-completions", "as I type", "", on, theme, {
                            let entity = entity.clone();
                            move |_, _, cx| entity.update(cx, |s, cx| s.set_completions(!on, cx))
                        })
                        .debug_selector(|| "ai-completions".into()),
                    ),
            )
            .child(
                div()
                    .text_size(small)
                    .text_color(theme.fg_subtle)
                    .child("Suggests code at the end of a line; Tab accepts, Escape dismisses. A small fast model works best."),
            );
        if self.completion_picker {
            let mut menu = div()
                .p_1()
                .rounded(theme.shape.control)
                .border(theme.shape.border)
                .border_color(theme.line)
                .bg(theme.bg_elev)
                .flex()
                .flex_col();
            for (provider, models) in store.choices() {
                menu = menu.child(
                    div()
                        .px_2()
                        .pt_1()
                        .text_size(crate::theme::text(10.5))
                        .text_color(theme.fg_subtle)
                        .child(provider.name.to_uppercase()),
                );
                for (model, label) in models {
                    let active = current.as_ref() == Some(&model);
                    let selector = format!("ai-completion-{}-{}", model.provider, model.model);
                    let entity = entity.clone();
                    menu = menu.child(
                        div()
                            .id(SharedString::from(selector.clone()))
                            .debug_selector(move || selector.clone())
                            .px_2()
                            .py_1()
                            .rounded(theme.shape.token)
                            .text_size(UI_FONT_SIZE)
                            .text_color(if active { theme.accent } else { theme.fg })
                            .hover(|d| d.bg(theme.accent_soft))
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                entity.update(cx, |s, cx| {
                                    s.choose(Role::Completion, model.clone(), cx)
                                });
                                this.completion_picker = false;
                                cx.notify();
                            })),
                    );
                }
            }
            block = block.child(menu);
        }
        block.into_any_element()
    }

    fn add(&mut self, _: &AddModel, _: &mut Window, cx: &mut Context<Self>) {
        let input = self.add_field.read(cx).text(cx);
        if input.trim().is_empty() {
            return;
        }
        self.store.update(cx, |s, cx| s.add(input, cx));
        self.add_field.update(cx, |e, cx| e.set_text("", false, cx));
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Add".into()),
        });
        let store = self.store.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                store
                    .update(cx, |s, cx| s.add(path.display().to_string(), cx))
                    .ok();
            }
        })
        .detach();
    }

    #[cfg(test)]
    pub fn provider_url_field(&self) -> Entity<Editor> {
        self.provider_url.clone()
    }

    #[cfg(test)]
    pub fn provider_key_field(&self) -> Entity<Editor> {
        self.provider_key.clone()
    }

    #[cfg(test)]
    pub fn add_field(&self) -> Entity<Editor> {
        self.add_field.clone()
    }

    pub fn shown(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| s.load(cx));
    }

    fn speed_text(speed: Speed) -> String {
        format!("{:.0} tok/s", speed.generate)
    }

    fn progress_bar(fraction: Option<f32>, theme: &Theme) -> impl IntoElement {
        div()
            .h(px(4.))
            .w_full()
            .rounded(theme.shape.token)
            .bg(theme.bg_sunken)
            .child(
                div()
                    .h_full()
                    .rounded(theme.shape.token)
                    .bg(theme.accent)
                    .w(gpui::relative(fraction.unwrap_or(0.).clamp(0., 1.))),
            )
    }

    fn render_benchmark(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let small = crate::theme::UI_FONT_SMALL;
        let mut block = div().px_3().pb_3().flex().flex_col().gap_2();
        if let Some(step) = &store.benchmark {
            let (label, fraction) = match step {
                BenchStep::Runtime(p) => ("Downloading llama.cpp", p.fraction()),
                BenchStep::Model(p) => ("Downloading the test model", p.fraction()),
                BenchStep::Measuring => ("Measuring speed", None),
            };
            let label = match fraction {
                Some(f) => format!("{label} {:.0}%", f * 100.),
                None => format!("{label}..."),
            };
            return block
                .child(
                    div()
                        .text_size(small)
                        .text_color(theme.fg_muted)
                        .child(label),
                )
                .child(Self::progress_bar(fraction, theme));
        }
        let entity = self.store.clone();
        match store.measured {
            None => {
                block = block
                    .child(
                        div()
                            .text_size(small)
                            .text_color(theme.fg_subtle)
                            .child("Measure this machine to find the best model it runs well. Downloads llama.cpp (12 MB) and a 0.8 GB test model."),
                    )
                    .child(
                        div().flex().child(
                            ui::button("ai-benchmark", "Run benchmark", true, theme, move |_, _, cx| {
                                entity.update(cx, |s, cx| s.run_benchmark(cx))
                            })
                            .debug_selector(|| "ai-benchmark".into()),
                        ),
                    );
            }
            Some(speed) => {
                let missing: Vec<Candidate> = store
                    .candidates()
                    .into_iter()
                    .filter(|c| !c.picks.is_empty() && !store.installed.contains(&c.model.id))
                    .filter(|c| !store.downloads.contains_key(&c.model.id))
                    .collect();
                let size: u64 = missing.iter().map(|c| c.model.size).sum();
                block = block.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(small)
                                .text_color(theme.fg_muted)
                                .child(format!(
                                    "Reads {:.0} and writes {:.0} tokens per second on the test model.",
                                    speed.prompt, speed.generate
                                )),
                        )
                        .child(ui::button("ai-benchmark", "Again", false, theme, {
                            let entity = entity.clone();
                            move |_, _, cx| entity.update(cx, |s, cx| s.run_benchmark(cx))
                        })),
                );
                if !missing.is_empty() {
                    block = block.child(
                        div().flex().child(
                            ui::button(
                                "ai-install-recommended",
                                format!("Install recommended ({})", format_size(size)),
                                true,
                                theme,
                                move |_, _, cx| {
                                    entity.update(cx, |s, cx| s.install_recommended(cx))
                                },
                            )
                            .debug_selector(|| "ai-install-recommended".into()),
                        ),
                    );
                } else if store.candidates().iter().all(|c| c.picks.is_empty()) {
                    block = block.child(div().text_size(small).text_color(theme.warning).child(
                        "No model runs fast enough here. Use a provider with your own key instead.",
                    ));
                }
            }
        }
        block
    }

    fn tag(label: &'static str, color: Hsla, theme: &Theme) -> impl IntoElement {
        div()
            .flex_none()
            .px_1()
            .rounded(theme.shape.token)
            .bg(theme.bg_sunken)
            .text_size(crate::theme::text(10.5))
            .text_color(color)
            .child(label)
    }

    fn render_row(&self, ix: usize, c: &Candidate, theme: &Theme, cx: &App) -> AnyElement {
        let store = self.store.read(cx);
        let model = c.model.clone();
        let id = model.id.clone();
        let installed = store.installed.contains(&id);
        let download = store.downloads.get(&id).cloned();
        let verifying = store.verifying.contains(&id);
        let verified = store.verified.get(&id).copied();
        let found = store.is_found(&id);
        let budget = store.hardware.as_ref().map_or(0, |h| h.model_budget());
        let detail = if !c.fits && !installed {
            format!(
                "Needs {}, {} available",
                format_size(model.memory()),
                format_size(budget)
            )
        } else {
            let speed = match (verified, c.speed) {
                (Some(v), _) => format!(" · {} measured", Self::speed_text(v)),
                (None, Some(p)) => format!(" · about {}", Self::speed_text(p)),
                _ => String::new(),
            };
            let active = if model.active < model.params * 0.9 {
                format!(" · {:.0}B, {:.1}B active", model.params, model.active)
            } else {
                format!(" · {:.1}B", model.params)
            };
            format!("{}{active}{speed}", format_size(model.size))
        };
        let entity = self.store.clone();
        let role_toggle = |role: Role, label: &'static str| {
            let entity = entity.clone();
            let active = store.roles.get(&role) == Some(&crate::ai_providers::ModelRef::local(&id));
            let selector = format!("ai-role-{label}-{id}");
            let model_id = id.clone();
            ui::toggle(
                SharedString::from(selector.clone()),
                label,
                "",
                active,
                theme,
                move |_, _, cx| entity.update(cx, |s, cx| s.set_role(role, &model_id, cx)),
            )
            .debug_selector(move || selector.clone())
        };
        let action: AnyElement = if let Some(progress) = &download {
            let fraction = progress.fraction();
            let model_id = id.clone();
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .w(px(36.))
                        .text_size(crate::theme::text(11.))
                        .text_color(theme.fg_muted)
                        .child(fraction.map_or("...".to_string(), |f| format!("{:.0}%", f * 100.))),
                )
                .child(ui::toggle(
                    SharedString::from(format!("ai-cancel-{id}")),
                    "cancel",
                    "",
                    false,
                    theme,
                    {
                        let entity = entity.clone();
                        move |_, _, cx| entity.update(cx, |s, cx| s.cancel(&model_id, cx))
                    },
                ))
                .into_any_element()
        } else if verifying {
            div()
                .text_size(crate::theme::text(11.))
                .text_color(theme.fg_subtle)
                .child("Measuring...")
                .into_any_element()
        } else if installed {
            // Files Solder did not download are only taken off the list.
            let remove_label = match model.source {
                Source::Hub { .. } => "delete",
                Source::File(_) => "remove",
            };
            let selector = format!("ai-delete-{id}");
            let removed = model.clone();
            let measured = model.clone();
            div()
                .flex()
                .items_center()
                .gap_0p5()
                .when(verified.is_none() && store.measured.is_some(), |d| {
                    let entity = entity.clone();
                    d.child(ui::toggle(
                        SharedString::from(format!("ai-measure-{id}")),
                        "measure",
                        "",
                        false,
                        theme,
                        move |_, _, cx| entity.update(cx, |s, cx| s.measure(measured.clone(), cx)),
                    ))
                })
                .child(role_toggle(Role::Chat, "chat"))
                .child(role_toggle(Role::Completion, "complete"))
                .when(!found, |d| {
                    d.child(
                        div()
                            .invisible()
                            .group_hover("ai-model", |s| s.visible())
                            .child(
                                ui::toggle(
                                    SharedString::from(selector.clone()),
                                    remove_label,
                                    "",
                                    false,
                                    theme,
                                    move |_, _, cx| {
                                        entity.update(cx, |s, cx| s.remove(removed.clone(), cx))
                                    },
                                )
                                .debug_selector(move || selector.clone()),
                            ),
                    )
                })
                .into_any_element()
        } else if c.fits || model.is_custom() {
            let selector = format!("ai-install-{id}");
            ui::button(
                SharedString::from(selector.clone()),
                "Install",
                !c.picks.is_empty(),
                theme,
                move |_, _, cx| entity.update(cx, |s, cx| s.install(model.clone(), cx)),
            )
            .debug_selector(move || selector.clone())
            .into_any_element()
        } else {
            div().into_any_element()
        };
        let origin = if found {
            Some("LM Studio")
        } else if c.model.is_custom() {
            Some("added")
        } else {
            None
        };
        div()
            .w_full()
            .px_1p5()
            .child(
                div()
                    .id(("ai-model", ix))
                    .debug_selector(move || format!("ai-model-{ix}"))
                    .group("ai-model")
                    .h(crate::theme::row(ROW_HEIGHT, cx))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(theme.shape.control)
                    .hover(|d| d.bg(theme.bg_elev))
                    .when(!c.fits && !installed, |d| d.opacity(0.55))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(UI_FONT_SIZE)
                                    .text_color(theme.fg)
                                    .child(div().truncate().child(c.model.name.clone()))
                                    // Advice for what to install; once installed,
                                    // the role toggles say what it does.
                                    .children(
                                        (!installed && c.picks.contains(&Role::Chat)).then(|| {
                                            Self::tag("best for chat", theme.accent, theme)
                                        }),
                                    )
                                    .children(
                                        (!installed
                                            && c.picks.contains(&Role::Completion)
                                            && !c.picks.contains(&Role::Chat))
                                        .then(|| {
                                            Self::tag("best to complete", theme.accent, theme)
                                        }),
                                    )
                                    .children(origin.map(|o| Self::tag(o, theme.fg_subtle, theme))),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(crate::theme::text(11.))
                                    .text_color(theme.fg_subtle)
                                    .child(detail),
                            )
                            .children(
                                download
                                    .as_ref()
                                    .map(|p| Self::progress_bar(p.fraction(), theme)),
                            ),
                    )
                    .child(div().flex_none().child(action)),
            )
            .into_any_element()
    }

    fn render_add(&self, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.add_field.focus_handle(cx).is_focused(window);
        let adding = self.store.read(cx).adding;
        div()
            .px_3()
            .pb_2()
            .flex()
            .items_center()
            .gap_1p5()
            .key_context("AiAddModel")
            .on_action(cx.listener(Self::add))
            .child(ui::text_field(self.add_field.clone(), focused, theme))
            .child(
                ui::button(
                    "ai-add",
                    if adding { "Adding..." } else { "Add" },
                    false,
                    theme,
                    cx.listener(|this, _, window, cx| this.add(&AddModel, window, cx)),
                )
                .debug_selector(|| "ai-add".into()),
            )
            .child(ui::button(
                "ai-browse",
                "File...",
                false,
                theme,
                cx.listener(|this, _, _, cx| this.browse(cx)),
            ))
            .into_any_element()
    }
}

impl Focusable for AiPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let view = self.view;
        let tabs = div()
            .px_3()
            .pb_2()
            .flex()
            .gap_1()
            .child(
                ui::toggle(
                    "ai-view-models",
                    "models",
                    "",
                    view == View::Models,
                    &theme,
                    cx.listener(|this, _, _, cx| this.show(View::Models, cx)),
                )
                .debug_selector(|| "ai-view-models".into()),
            )
            .child(
                ui::toggle(
                    "ai-view-providers",
                    "providers",
                    "",
                    view == View::Providers,
                    &theme,
                    cx.listener(|this, _, _, cx| this.show(View::Providers, cx)),
                )
                .debug_selector(|| "ai-view-providers".into()),
            )
            .child(
                ui::toggle(
                    "ai-view-servers",
                    "context servers",
                    "",
                    view == View::Servers,
                    &theme,
                    cx.listener(|this, _, _, cx| this.show(View::Servers, cx)),
                )
                .debug_selector(|| "ai-view-servers".into()),
            );
        if view == View::Servers {
            let servers = self.render_servers(&theme, cx);
            return div()
                .key_context("AiPanel")
                .track_focus(&self.focus)
                .size_full()
                .flex()
                .flex_col()
                .child(tabs)
                .child(servers)
                .into_any_element();
        }
        if view == View::Providers {
            let providers = self.render_providers(window, &theme, cx);
            let error = self.store.read(cx).error.clone();
            return div()
                .key_context("AiPanel")
                .track_focus(&self.focus)
                .size_full()
                .flex()
                .flex_col()
                .child(tabs)
                .children(error.map(|e| {
                    div()
                        .px_3()
                        .pb_2()
                        .text_size(crate::theme::UI_FONT_SMALL)
                        .text_color(theme.error)
                        .child(e)
                }))
                .child(providers)
                .into_any_element();
        }
        let add = self.render_add(window, &theme, cx);
        let store = self.store.read(cx);
        let small = crate::theme::UI_FONT_SMALL;
        let header = match &store.hardware {
            None => div()
                .px_3()
                .text_size(small)
                .text_color(theme.fg_subtle)
                .child("Reading this machine..."),
            Some(hw) => div()
                .px_3()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(UI_FONT_SIZE)
                        .text_color(theme.fg)
                        .child(hw.summary()),
                )
                .child(
                    div()
                        .text_size(small)
                        .text_color(theme.fg_subtle)
                        .child(format!(
                            "{} for models{}",
                            format_size(hw.model_budget()),
                            store
                                .free_disk
                                .map(|f| format!(" · {} free on disk", format_size(f)))
                                .unwrap_or_default()
                        )),
                ),
        };
        let error = store.error.clone();
        let loaded = store.hardware.is_some();
        let candidates = store.candidates();
        let count = candidates.len();
        div()
            .key_context("AiPanel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(tabs)
            .child(header.pb_3())
            .when(loaded, |d| {
                d.child(self.render_benchmark(&theme, cx)).child(add)
            })
            .children(error.map(|e| {
                div()
                    .px_3()
                    .pb_2()
                    .text_size(small)
                    .text_color(theme.error)
                    .child(e)
            }))
            .child(
                uniform_list(
                    "ai-models",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let theme = cx.theme().clone();
                        range
                            .map(|ix| this.render_row(ix, &candidates[ix], &theme, cx))
                            .collect()
                    }),
                )
                .flex_1(),
            )
            .into_any_element()
    }
}
