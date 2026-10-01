//! The AI tab: this machine, the benchmark, and the models it can run, with
//! install, delete, models added by hand and which model serves chat and
//! completions.

use ai::{Candidate, Role, Source, Speed, format_size};
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, Hsla, KeyBinding, PathPromptOptions,
    SharedString, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::{
    ai_store::{AiStore, BenchStep},
    editor::Editor,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

const ROW_HEIGHT: gpui::Pixels = px(54.);

actions!(ai_panel, [AddModel]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("enter", AddModel, Some("AiAddModel"))]);
}

pub struct AiPanel {
    store: Entity<AiStore>,
    add_field: Entity<Editor>,
    focus: FocusHandle,
}

impl AiPanel {
    pub fn new(store: Entity<AiStore>, cx: &mut Context<Self>) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        let add_field = cx.new(|cx| Editor::single_line("org/model-GGUF or a .gguf path", cx));
        Self {
            store,
            add_field,
            focus: cx.focus_handle(),
        }
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
            .rounded(px(6.))
            .bg(theme.bg_sunken)
            .child(
                div()
                    .h_full()
                    .rounded(px(6.))
                    .bg(theme.accent)
                    .w(gpui::relative(fraction.unwrap_or(0.).clamp(0., 1.))),
            )
    }

    fn render_benchmark(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let small = UI_FONT_SIZE - px(1.);
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
            .rounded(px(6.))
            .bg(theme.bg_sunken)
            .text_size(px(10.5))
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
            let active = store.roles.get(&role) == Some(&id);
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
                        .text_size(px(11.))
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
                .text_size(px(11.))
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
                    .h(ROW_HEIGHT)
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(8.))
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
                                    .children(
                                        c.picks.contains(&Role::Chat).then(|| {
                                            Self::tag("best for chat", theme.accent, theme)
                                        }),
                                    )
                                    .children(
                                        (c.picks.contains(&Role::Completion)
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
                                    .text_size(px(11.))
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
        let add = self.render_add(window, &theme, cx);
        let store = self.store.read(cx);
        let small = UI_FONT_SIZE - px(1.);
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
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .text_size(px(11.))
                    .text_color(theme.fg_subtle)
                    .child("LOCAL MODELS"),
            )
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
    }
}
