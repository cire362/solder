//! The Import settings window: the editors found on this machine and, for
//! the one picked, what can come over, each item with a checkbox.

use std::{collections::HashSet, path::PathBuf};

use gpui::{
    AnyElement, App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, KeyBinding,
    SharedString, Window, actions, div, prelude::*, px, uniform_list,
};
use import::{Plan, Roots};

use crate::{
    import_settings::{self, Choice},
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(import_view, [Close, Confirm]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Close, Some("ImportView")),
        KeyBinding::new("enter", Confirm, Some("ImportView")),
    ]);
}

const ROW: gpui::Pixels = px(26.);

/// Something the user can take or leave.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Item {
    Setting(usize),
    Theme,
    Preset,
    Custom,
}

#[derive(Clone)]
enum Row {
    Header(&'static str),
    Item {
        item: Item,
        label: SharedString,
        detail: SharedString,
        /// Why it cannot be imported; it is shown unchecked.
        kept: Option<SharedString>,
    },
    Note(SharedString),
}

pub struct ImportView {
    /// Solder's config folder, where the import is written.
    target: PathBuf,
    plans: Vec<Plan>,
    loaded: bool,
    source: usize,
    /// Unchecked items, by source.
    off: HashSet<(usize, Item)>,
    /// The user's own settings, which an import leaves alone.
    changed: serde_json::Map<String, serde_json::Value>,
    fonts: Vec<String>,
    /// What the last import did, or why it failed.
    status: Option<(SharedString, bool)>,
    focus: FocusHandle,
}

impl ImportView {
    pub fn new(roots: Roots, target: PathBuf, cx: &mut Context<Self>) -> Self {
        let dir = target.clone();
        cx.spawn(async move |this, cx| {
            let (plans, changed) = cx
                .background_executor()
                .spawn(async move {
                    let mac = cfg!(target_os = "macos");
                    let plans: Vec<Plan> = import::detect(&roots)
                        .iter()
                        .map(|source| import::plan(source, &roots, mac))
                        .collect();
                    (plans, import_settings::changed_settings(&dir))
                })
                .await;
            this.update(cx, |this, cx| {
                this.plans = plans;
                this.changed = changed;
                this.loaded = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            target,
            plans: Vec::new(),
            loaded: false,
            source: 0,
            off: HashSet::new(),
            changed: Default::default(),
            fonts: cx.text_system().all_font_names(),
            status: None,
            focus: cx.focus_handle(),
        }
    }

    #[cfg(test)]
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    #[cfg(test)]
    pub fn status(&self) -> Option<String> {
        self.status.as_ref().map(|(text, _)| text.to_string())
    }

    /// The rows for the editor picked.
    fn rows(&self) -> Vec<Row> {
        let Some(plan) = self.plans.get(self.source) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        if !plan.settings.is_empty() {
            rows.push(Row::Header("SETTINGS"));
        }
        for (i, setting) in plan.settings.iter().enumerate() {
            let kept = if let Some(mine) = self.changed.get(setting.key) {
                let mine = mine
                    .as_str()
                    .map_or_else(|| mine.to_string(), str::to_string);
                Some(format!("kept: you set {mine}").into())
            } else if setting.key == "buffer_font_family"
                && !self.fonts.iter().any(|f| f == &setting.shown())
            {
                Some("this font is not installed".into())
            } else {
                None
            };
            rows.push(Row::Item {
                item: Item::Setting(i),
                label: setting.label.into(),
                detail: setting.shown().into(),
                kept,
            });
        }
        if let Some(name) = &plan.theme_name {
            rows.push(Row::Header("THEME"));
            let kept = if plan.theme.is_none() {
                Some("its file was not found".into())
            } else {
                self.changed.get("theme").map(|mine| {
                    format!("kept: you set {}", mine.as_str().unwrap_or_default()).into()
                })
            };
            rows.push(Row::Item {
                item: Item::Theme,
                label: name.clone().into(),
                detail: match &plan.theme {
                    Some(theme) => theme.appearance.clone().into(),
                    None => "".into(),
                },
                kept,
            });
        }
        if plan.preset_name.is_some() || !plan.custom.bindings.is_empty() {
            rows.push(Row::Header("KEYS"));
        }
        if let Some(name) = plan.preset_name {
            rows.push(Row::Item {
                item: Item::Preset,
                label: name.into(),
                detail: format!("{} bindings", plan.preset.len()).into(),
                kept: None,
            });
        }
        let (bound, skipped) = (plan.custom.bindings.len(), plan.custom.skipped.len());
        if bound > 0 {
            rows.push(Row::Item {
                item: Item::Custom,
                label: "Your own bindings".into(),
                detail: match skipped {
                    0 => format!("{bound}"),
                    n => format!("{bound}, {n} have no equivalent"),
                }
                .into(),
                kept: None,
            });
        } else if skipped > 0 {
            rows.push(Row::Note(
                format!("Your {skipped} own bindings have no equivalent here").into(),
            ));
        }
        if !plan.hints.is_empty() {
            rows.push(Row::Header("ALREADY BUILT IN"));
        }
        for hint in &plan.hints {
            rows.push(Row::Note(
                format!("{}: {}", hint.extension, hint.instead).into(),
            ));
        }
        rows
    }

    fn checked(&self, item: Item, kept: bool) -> bool {
        !kept && !self.off.contains(&(self.source, item))
    }

    fn toggle(&mut self, item: Item, cx: &mut Context<Self>) {
        let key = (self.source, item);
        if !self.off.remove(&key) {
            self.off.insert(key);
        }
        cx.notify();
    }

    /// What is checked for the editor picked.
    fn choice(&self) -> Option<Choice> {
        let plan = self.plans.get(self.source)?;
        let mut choice = Choice {
            source: plan.source.name.clone(),
            ..Default::default()
        };
        let (mut preset, mut custom) = (false, false);
        for row in self.rows() {
            let Row::Item { item, kept, .. } = row else {
                continue;
            };
            if !self.checked(item, kept.is_some()) {
                continue;
            }
            match item {
                Item::Setting(i) => choice.settings.push(plan.settings[i].clone()),
                Item::Theme => choice.theme = plan.theme.clone(),
                Item::Preset => preset = true,
                Item::Custom => custom = true,
            }
        }
        // The editor's keys first, so the user's own replace them.
        if preset {
            choice.bindings.extend(plan.preset.iter().cloned());
        }
        if custom {
            choice.bindings.extend(plan.custom.bindings.iter().cloned());
        }
        Some(choice)
    }

    fn import(&mut self, cx: &mut Context<Self>) {
        let Some(choice) = self.choice() else {
            return;
        };
        let (target, source) = (self.target.clone(), choice.source.clone());
        cx.spawn(async move |this, cx| {
            let dir = target.clone();
            let (done, changed) = cx
                .background_executor()
                .spawn(async move {
                    let done = import_settings::apply(&dir, &choice);
                    (done, import_settings::changed_settings(&dir))
                })
                .await;
            this.update(cx, |this, cx| {
                this.status = Some(match done {
                    Ok(done) if done.is_empty() => ("Nothing was picked".into(), false),
                    Ok(done) => (
                        format!("Imported from {source}: {}", done.join(", ")).into(),
                        false,
                    ),
                    Err(error) => (error.into(), true),
                });
                // What just came over is the user's now.
                this.changed = changed;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn close(&mut self, _: &Close, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        self.import(cx);
    }

    fn checkbox(checked: bool, enabled: bool, theme: &Theme) -> AnyElement {
        div()
            .flex_none()
            .size(px(14.))
            .rounded(px(4.))
            .border_1()
            .border_color(if checked { theme.accent } else { theme.line })
            .when(checked, |d| d.bg(theme.accent))
            .when(!enabled, |d| d.opacity(0.4))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(10.))
            .text_color(theme.accent_fg)
            .when(checked, |d| d.child("✓"))
            .into_any_element()
    }
}

impl EventEmitter<DismissEvent> for ImportView {}

impl Focusable for ImportView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ImportView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = self.rows();
        let count = rows.len();
        let states: Vec<bool> = rows
            .iter()
            .map(|row| match row {
                Row::Item { item, kept, .. } => self.checked(*item, kept.is_some()),
                _ => false,
            })
            .collect();
        let view = cx.entity();
        let list = uniform_list("import-rows", count, move |range, _, cx| {
            let theme = cx.theme().clone();
            range
                .map(|i| match rows[i].clone() {
                    Row::Header(text) => div()
                        .h(ROW)
                        .px_3()
                        .flex()
                        .items_end()
                        .pb_1()
                        .text_size(px(10.5))
                        .text_color(theme.fg_subtle)
                        .child(text)
                        .into_any_element(),
                    Row::Note(text) => div()
                        .h(ROW)
                        .px_3()
                        .flex()
                        .items_center()
                        .text_size(UI_FONT_SIZE)
                        .text_color(theme.fg_muted)
                        .child(div().truncate().child(text))
                        .into_any_element(),
                    Row::Item {
                        item,
                        label,
                        detail,
                        kept,
                    } => {
                        let enabled = kept.is_none();
                        let view = view.clone();
                        div()
                            .id(("import-item", i))
                            .debug_selector(move || format!("import-item-{i}"))
                            .w_full()
                            .h(ROW)
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(UI_FONT_SIZE)
                            .when(enabled, |d| d.hover(|d| d.bg(theme.bg_elev)))
                            .child(Self::checkbox(states[i], enabled, &theme))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(if enabled { theme.fg } else { theme.fg_subtle })
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.fg_muted)
                                    .child(detail),
                            )
                            .children(kept.map(|why| {
                                div().flex_none().text_color(theme.fg_subtle).child(why)
                            }))
                            .when(enabled, |d| {
                                d.on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| this.toggle(item, cx))
                                })
                            })
                            .into_any_element()
                    }
                })
                .collect()
        })
        .h(ROW * count.clamp(1, 13) as f32);

        let tabs = self.plans.iter().enumerate().map(|(i, plan)| {
            let active = i == self.source;
            div()
                .id(("import-source", i))
                .debug_selector(move || format!("import-source-{i}"))
                .h(px(24.))
                .px_2()
                .flex()
                .items_center()
                .rounded(px(8.))
                .text_size(UI_FONT_SIZE)
                .text_color(if active { theme.fg } else { theme.fg_subtle })
                .when(active, |d| d.bg(theme.bg_elev))
                .hover(|d| d.text_color(theme.fg))
                .child(plan.source.name.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.source = i;
                    this.status = None;
                    cx.notify();
                }))
        });
        let status = self.status.clone();
        div()
            .key_context("ImportView")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::confirm))
            .w(px(560.))
            .flex()
            .flex_col()
            .rounded(px(16.))
            .border_1()
            .border_color(theme.line)
            .bg(theme.bg)
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .h(px(40.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.line)
                    .child(
                        div()
                            .mr_2()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg)
                            .child("Import settings from"),
                    )
                    .children(tabs),
            )
            .map(|d| {
                if !self.loaded {
                    return d.child(
                        div()
                            .p_3()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg_subtle)
                            .child("Looking for other editors..."),
                    );
                }
                if self.plans.is_empty() {
                    return d.child(
                        div()
                            .p_3()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg_subtle)
                            .child("No settings of VS Code, Cursor, Zed or a JetBrains IDE were found."),
                    );
                }
                d.child(div().py_1().child(list)).child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .border_t_1()
                        .border_color(theme.line)
                        .child(
                            ui::button("import-apply", "Import", true, &theme, {
                                let view = cx.entity();
                                move |_, _, cx| view.update(cx, |this, cx| this.import(cx))
                            })
                            .debug_selector(|| "import-apply".into()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.5))
                                .text_color(match &status {
                                    Some((_, true)) => theme.error,
                                    Some(_) => theme.fg,
                                    None => theme.fg_subtle,
                                })
                                .child(match status {
                                    Some((text, _)) => text,
                                    None => "Adds to your settings. Nothing you changed in Solder is replaced.".into(),
                                }),
                        ),
                )
            })
    }
}
