//! The Import settings window: the editors found on this machine and, for
//! the one picked, what can come over, each item with a checkbox.

use crate::lang::{t, tf};
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

fn setting_label(label: &'static str) -> &'static str {
    match label {
        "Font" => t("Font"),
        "Font size" => t("Font size"),
        "Line height" => t("Line height"),
        "Indent" => t("Indent"),
        "Format on save" => t("Format on save"),
        other => other,
    }
}

fn setting_value(setting: &import::Setting) -> String {
    match &setting.value {
        serde_json::Value::Bool(true) => t("on").into(),
        serde_json::Value::Bool(false) => t("off").into(),
        _ => setting.shown(),
    }
}

fn appearance_label(appearance: &str) -> &str {
    match appearance {
        "dark" => t("dark"),
        "light" => t("light"),
        other => other,
    }
}

fn preset_label(name: &'static str) -> &'static str {
    match name {
        "VS Code keys" => t("VS Code keys"),
        "JetBrains keys" => t("JetBrains keys"),
        other => other,
    }
}

fn hint_label(hint: &'static str) -> &'static str {
    match hint {
        "The Git tab: changes, staging by line, commits, conflicts" => {
            t("The Git tab: changes, staging by line, commits, conflicts")
        }
        "The API tab sends requests from .http files" => {
            t("The API tab sends requests from .http files")
        }
        "The Services tab runs the stack and lists containers" => {
            t("The Services tab runs the stack and lists containers")
        }
        "The Database tab: Postgres, MySQL, SQLite, Redis, MongoDB" => {
            t("The Database tab: Postgres, MySQL, SQLite, Redis, MongoDB")
        }
        "Solder AI: completions, chat and agents, local or with your key" => {
            t("Solder AI: completions, chat and agents, local or with your key")
        }
        "rust-analyzer runs when it is on your PATH" => {
            t("rust-analyzer runs when it is on your PATH")
        }
        "gopls runs when it is on your PATH" => t("gopls runs when it is on your PATH"),
        "pyright runs when it is on your PATH" => t("pyright runs when it is on your PATH"),
        "The debugger runs JavaScript and TypeScript with js-debug" => {
            t("The debugger runs JavaScript and TypeScript with js-debug")
        }
        "The debugger runs a server and its page in one session" => {
            t("The debugger runs a server and its page in one session")
        }
        other => other,
    }
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
            rows.push(Row::Header(t("SETTINGS")));
        }
        for (i, setting) in plan.settings.iter().enumerate() {
            let kept = if let Some(mine) = self.changed.get(setting.key) {
                let mine = mine
                    .as_str()
                    .map_or_else(|| mine.to_string(), str::to_string);
                Some(tf("kept: you set {}", &[&mine]).into())
            } else if setting.key == "buffer_font_family"
                && !self.fonts.iter().any(|f| f == &setting.shown())
            {
                Some(t("this font is not installed").into())
            } else {
                None
            };
            rows.push(Row::Item {
                item: Item::Setting(i),
                label: setting_label(setting.label).into(),
                detail: setting_value(setting).into(),
                kept,
            });
        }
        if let Some(name) = &plan.theme_name {
            rows.push(Row::Header(t("THEME")));
            let kept = if plan.theme.is_none() {
                Some(t("its file was not found").into())
            } else {
                self.changed.get("theme").map(|mine| {
                    tf("kept: you set {}", &[&mine.as_str().unwrap_or_default()]).into()
                })
            };
            rows.push(Row::Item {
                item: Item::Theme,
                label: name.clone().into(),
                detail: match &plan.theme {
                    Some(theme) => appearance_label(&theme.appearance).to_string().into(),
                    None => "".into(),
                },
                kept,
            });
        }
        if plan.preset_name.is_some() || !plan.custom.bindings.is_empty() {
            rows.push(Row::Header(t("KEYS")));
        }
        if let Some(name) = plan.preset_name {
            rows.push(Row::Item {
                item: Item::Preset,
                label: preset_label(name).into(),
                detail: tf("{} bindings", &[&plan.preset.len()]).into(),
                kept: None,
            });
        }
        let (bound, skipped) = (plan.custom.bindings.len(), plan.custom.skipped.len());
        if bound > 0 {
            rows.push(Row::Item {
                item: Item::Custom,
                label: t("Your own bindings").into(),
                detail: match skipped {
                    0 => format!("{bound}"),
                    n => tf("{}, {} have no equivalent", &[&bound, &n]),
                }
                .into(),
                kept: None,
            });
        } else if skipped > 0 {
            rows.push(Row::Note(
                tf("Your {} own bindings have no equivalent here", &[&skipped]).into(),
            ));
        }
        if !plan.hints.is_empty() {
            rows.push(Row::Header(t("ALREADY BUILT IN")));
        }
        for hint in &plan.hints {
            rows.push(Row::Note(
                format!("{}: {}", hint.extension, hint_label(hint.instead)).into(),
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
                    Ok(done) if done.is_empty() => (t("Nothing was picked").into(), false),
                    Ok(done) => (
                        tf("Imported from {}: {}", &[&source, &done.join(", ")]).into(),
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
            .border(theme.shape.border)
            .border_color(if checked { theme.accent } else { theme.line })
            .when(checked, |d| d.bg(theme.accent))
            .when(!enabled, |d| d.opacity(0.4))
            .flex()
            .items_center()
            .justify_center()
            .text_size(crate::theme::text(10.))
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
                        .h(crate::theme::row(ROW, cx))
                        .px_3()
                        .flex()
                        .items_end()
                        .pb_1()
                        .text_size(crate::theme::text(10.5))
                        .text_color(theme.fg_subtle)
                        .child(text)
                        .into_any_element(),
                    Row::Note(text) => div()
                        .h(crate::theme::row(ROW, cx))
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
                            .h(crate::theme::row(ROW, cx))
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
        .h(crate::theme::row(ROW, cx) * count.clamp(1, 13) as f32);

        let tabs = self.plans.iter().enumerate().map(|(i, plan)| {
            let active = i == self.source;
            div()
                .id(("import-source", i))
                .debug_selector(move || format!("import-source-{i}"))
                .h(px(24.))
                .px_2()
                .flex()
                .items_center()
                .rounded(theme.shape.control)
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
            .rounded(theme.shape.panel)
            .border(theme.shape.border)
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
                    .border_b(theme.shape.border)
                    .border_color(theme.line)
                    .child(
                        div()
                            .mr_2()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg)
                            .child(t("Import settings from")),
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
                            .child(t("Looking for other editors...")),
                    );
                }
                if self.plans.is_empty() {
                    return d.child(
                        div()
                            .p_3()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg_subtle)
                            .child(t("No settings of VS Code, Cursor, Zed or a JetBrains IDE were found.")),
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
                        .border_t(theme.shape.border)
                        .border_color(theme.line)
                        .child(
                            ui::button("import-apply", t("Import"), true, &theme, {
                                let view = cx.entity();
                                move |_, _, cx| view.update(cx, |this, cx| this.import(cx))
                            })
                            .debug_selector(|| "import-apply".into()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(crate::theme::text(11.5))
                                .text_color(match &status {
                                    Some((_, true)) => theme.error,
                                    Some(_) => theme.fg,
                                    None => theme.fg_subtle,
                                })
                                .child(match status {
                                    Some((text, _)) => text,
                                    None => t("Adds to your settings. Nothing you changed in Solder is replaced.").into(),
                                }),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod localization_tests {
    use super::*;

    #[test]
    fn russian_import_labels_leave_setting_values_and_theme_identifiers_intact() {
        let setting = import::Setting {
            key: "format_on_save",
            label: "Format on save",
            value: serde_json::json!(true),
        };
        let theme = import::ThemeFile {
            appearance: "dark".into(),
            ..Default::default()
        };
        crate::lang::set(crate::lang::Language::Russian);
        assert_eq!(setting_label(setting.label), "Форматировать при сохранении");
        assert_eq!(setting_value(&setting), "вкл.");
        assert_eq!(appearance_label(&theme.appearance), "тёмная");
        assert_eq!(setting.key, "format_on_save");
        assert_eq!(setting.value, serde_json::json!(true));
        assert_eq!(theme.appearance, "dark");
        assert_eq!(setting.shown(), "on");
        crate::lang::set(crate::lang::Language::English);
        assert_eq!(setting_value(&setting), "on");
    }
}
