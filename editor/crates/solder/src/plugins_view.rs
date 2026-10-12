//! The Plugins window: what is installed, with each plugin's permissions
//! shown before it is enabled, and how the running ones behave.

use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, SharedString, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::{
    plugin_store::{PluginState, PluginStore},
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(plugins_view, [Close, SelectNext, SelectPrevious, Toggle]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Close, Some("PluginsView")),
        KeyBinding::new("down", SelectNext, Some("PluginsView")),
        KeyBinding::new("up", SelectPrevious, Some("PluginsView")),
        KeyBinding::new("enter", Toggle, Some("PluginsView")),
    ]);
}

const ROW: gpui::Pixels = px(30.);

pub struct PluginsView {
    store: Entity<PluginStore>,
    selected: usize,
    focus: FocusHandle,
}

impl PluginsView {
    pub fn new(store: Entity<PluginStore>, cx: &mut Context<Self>) -> Self {
        // What is in the folder now, in case it changed since the start.
        store.update(cx, |s, cx| s.scan(cx));
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self {
            store,
            selected: 0,
            focus: cx.focus_handle(),
        }
    }

    fn close(&mut self, _: &Close, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.store.read(cx).installed.len();
        if count > 0 {
            self.selected = (self.selected + 1).min(count - 1);
            cx.notify();
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        cx.notify();
    }

    fn toggle(&mut self, _: &Toggle, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_selected(cx);
    }

    /// Enables the selected plugin with the permissions shown, or turns it
    /// off.
    fn toggle_selected(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            let Some(name) = store.installed.get(self.selected).map(|p| p.name.clone()) else {
                return;
            };
            match store.state(&name) {
                PluginState::Enabled => store.disable(&name, cx),
                PluginState::Disabled | PluginState::Changed => store.enable(&name, cx),
                PluginState::Invalid(_) => {}
            }
        });
    }

    fn badge(text: &'static str, color: gpui::Hsla, theme: &Theme) -> AnyElement {
        div()
            .flex_none()
            .px_1p5()
            .rounded(theme.shape.token)
            .bg(theme.bg_sunken)
            .text_size(crate::theme::text(11.))
            .text_color(color)
            .child(text)
            .into_any_element()
    }

    /// The selected plugin: what it is, what enabling it allows, and how it
    /// has behaved.
    fn render_details(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let Some(installed) = store.installed.get(self.selected) else {
            return div().into_any_element();
        };
        let state = store.state(&installed.name);
        let stats = store.stats(&installed.name);
        let line = |text: SharedString, color: gpui::Hsla| {
            div().text_size(UI_FONT_SIZE).text_color(color).child(text)
        };
        let mut details = div()
            .flex_1()
            .min_h_0()
            .id("plugin-details")
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_1();
        match &installed.manifest {
            Err(error) => details = details.child(line(error_label(error).into(), theme.error)),
            Ok(manifest) => {
                if !manifest.description.is_empty() {
                    details = details.child(line(manifest.description.clone().into(), theme.fg));
                }
                details = details.child(
                    div()
                        .pt_1()
                        .text_size(crate::theme::text(10.5))
                        .text_color(theme.fg_subtle)
                        .child(match state {
                            PluginState::Enabled => crate::lang::t("ALLOWED"),
                            _ => crate::lang::t("ENABLING IT ALLOWS IT TO"),
                        }),
                );
                if manifest.permissions.is_empty() {
                    details = details.child(line(
                        crate::lang::t("Nothing outside its own memory").into(),
                        theme.fg_muted,
                    ));
                }
                for permission in &manifest.permissions {
                    details =
                        details.child(line(permission_label(permission).into(), theme.fg_muted));
                }
            }
        }
        match &state {
            PluginState::Changed => {
                details = details.child(line(
                    crate::lang::t(
                        "Changed since it was enabled. Check the list and enable it again.",
                    )
                    .into(),
                    theme.warning,
                ));
            }
            PluginState::Invalid(reason) if installed.manifest.is_ok() => {
                details = details.child(line(reason.clone().into(), theme.error));
            }
            _ => {}
        }
        if let Some(stats) = stats {
            if let Some(error) = &stats.error {
                details = details.child(line(error_label(error).into(), theme.error));
            }
            if stats.events > 0 {
                details = details.child(line(
                    crate::lang::tf(
                        "{} events. Last took {} ms, the slowest {} ms.",
                        &[
                            &stats.events,
                            &format!("{:.1}", stats.last_ms),
                            &format!("{:.1}", stats.slowest_ms),
                        ],
                    )
                    .into(),
                    theme.fg_subtle,
                ));
            }
            if stats.over_budget > 0 || stats.stopped > 0 {
                details = details.child(line(
                    crate::lang::tf(
                        "Over the typing budget {} times, stopped at the limit {} times.",
                        &[&stats.over_budget, &stats.stopped],
                    )
                    .into(),
                    if stats.slow() {
                        theme.warning
                    } else {
                        theme.fg_subtle
                    },
                ));
            }
            if let Some(failure) = &stats.last_failure {
                details = details.child(line(
                    crate::lang::tf(
                        "{} failed. Last: {}",
                        &[&stats.failures, &error_label(failure)],
                    )
                    .into(),
                    theme.error,
                ));
            }
            for entry in stats.log.iter().rev().take(5).rev() {
                details = details.child(
                    div()
                        .font_family(crate::theme::CODE_FONT)
                        .text_size(crate::theme::text(11.5))
                        .text_color(theme.fg_subtle)
                        .child(entry.clone()),
                );
            }
        }
        details.into_any_element()
    }
}

/// Permission names are stored in approvals; only this description is localized.
fn permission_label(permission: &plugin::Permission) -> String {
    match permission {
        plugin::Permission::StatusBar => crate::lang::t("Show text in the status bar").into(),
        plugin::Permission::EditorRead => {
            crate::lang::t("Read the file in front and its selection").into()
        }
        plugin::Permission::EditorWrite => crate::lang::t("Change the file in front").into(),
        plugin::Permission::FsRead => {
            crate::lang::t("Read files of the project (not .env or files kept from AI)").into()
        }
        plugin::Permission::Http(host) => crate::lang::tf("Send requests to {}", &[&host]),
    }
}

/// Plugin and runtime details stay as supplied; editor-owned wrappers are display text.
fn error_label(error: &str) -> String {
    error_label_inner(error, true)
}

fn error_label_inner(error: &str, nested: bool) -> String {
    let detail_label = |detail: &str| {
        if nested {
            error_label_inner(detail, false)
        } else {
            detail.to_string()
        }
    };
    match error {
        "The answer is too large" => return crate::lang::t("The answer is too large").into(),
        "Only http and https URLs with a host name" => {
            return crate::lang::t("Only http and https URLs with a host name").into();
        }
        "No project is open" => return crate::lang::t("No project is open").into(),
        "The editor is closing" => return crate::lang::t("The editor is closing").into(),
        "The editor did not answer" => return crate::lang::t("The editor did not answer").into(),
        "A path from the project's root, without .." => {
            return crate::lang::t("A path from the project's root, without ..").into();
        }
        "The file is outside the project" => {
            return crate::lang::t("The file is outside the project").into();
        }
        "The file is kept private (.env or .solderignore)" => {
            return crate::lang::t("The file is kept private (.env or .solderignore)").into();
        }
        "The file is larger than 4 MB" => {
            return crate::lang::t("The file is larger than 4 MB").into();
        }
        "The file is not text" => return crate::lang::t("The file is not text").into(),
        "No file is open" => return crate::lang::t("No file is open").into(),
        "The file is read-only" => return crate::lang::t("The file is read-only").into(),
        "The range is not inside the text" => {
            return crate::lang::t("The range is not inside the text").into();
        }
        "Not answered by the editor" => return crate::lang::t("Not answered by the editor").into(),
        "A plugin's name is lowercase letters, digits and dashes" => {
            return crate::lang::t("A plugin's name is lowercase letters, digits and dashes")
                .into();
        }
        "The module has no solder_event function" => {
            return crate::lang::t("The module has no solder_event function").into();
        }
        "The program exports no memory" => {
            return crate::lang::t("The program exports no memory").into();
        }
        "The plugin is not waiting for an event" => {
            return crate::lang::t("The plugin is not waiting for an event").into();
        }
        "Stopped: one event ran past the limit" => {
            return crate::lang::t("Stopped: one event ran past the limit").into();
        }
        "The plugin ended" => return crate::lang::t("The plugin ended").into(),
        "The script ended" => return crate::lang::t("The script ended").into(),
        _ => {}
    }
    if let Some(detail) = error.strip_prefix("Cannot read plugin.json: ") {
        return crate::lang::tf("Cannot read plugin.json: {}", &[&detail]);
    }
    if let Some(detail) = error.strip_prefix("plugin.json is not valid: ") {
        return crate::lang::tf("plugin.json is not valid: {}", &[&detail]);
    }
    if let Some(name) = error
        .strip_prefix("Unknown permission \"")
        .and_then(|name| name.strip_suffix('"'))
    {
        return crate::lang::tf("Unknown permission \"{}\"", &[&name]);
    }
    if let Some(name) = error
        .strip_prefix("Unknown event \"")
        .and_then(|name| name.strip_suffix('"'))
    {
        return crate::lang::tf("Unknown event \"{}\"", &[&name]);
    }
    if let Some(name) = error
        .strip_prefix('"')
        .and_then(|name| name.strip_suffix("\" does not name a host"))
    {
        return crate::lang::tf("\"{}\" does not name a host", &[&name]);
    }
    if let Some(detail) = error.strip_prefix("Could not start: ") {
        return crate::lang::tf("Could not start: {}", &[&detail_label(detail)]);
    }
    if let Some(detail) = error.strip_prefix("Could not load: ") {
        return crate::lang::tf("Could not load: {}", &[&detail_label(detail)]);
    }
    if let Some(detail) = error.strip_prefix("Could not restart: ") {
        return crate::lang::tf("Could not restart: {}", &[&detail_label(detail)]);
    }
    if let Some(detail) = error.strip_prefix("The plugin failed: ") {
        return crate::lang::tf("The plugin failed: {}", &[&detail_label(detail)]);
    }
    if let Some(detail) = error.strip_prefix("The script failed: ") {
        return crate::lang::tf("The script failed: {}", &[&detail_label(detail)]);
    }
    if let Some(code) = error.strip_prefix("The plugin ended with code ") {
        return crate::lang::tf("The plugin ended with code {}", &[&code]);
    }
    if let Some(code) = error.strip_prefix("The script ended with code ") {
        return crate::lang::tf("The script ended with code {}", &[&code]);
    }
    if let Some(path) = error.strip_prefix("No file ") {
        return crate::lang::tf("No file {}", &[&path]);
    }
    if let Some(path) = error.strip_suffix(" is not the file in front") {
        return crate::lang::tf("{} is not the file in front", &[&path]);
    }
    if let Some(permission) = error.strip_prefix("The plugin did not declare the permission ") {
        return crate::lang::tf(
            "The plugin did not declare the permission {}",
            &[&permission],
        );
    }
    error.to_string()
}

impl EventEmitter<DismissEvent> for PluginsView {}

impl Focusable for PluginsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PluginsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        self.selected = self.selected.min(store.installed.len().saturating_sub(1));
        let selected = self.selected;
        let rows: Vec<(SharedString, SharedString, PluginState, bool)> = store
            .installed
            .iter()
            .map(|p| {
                let mut version = p
                    .manifest
                    .as_ref()
                    .map(|m| m.version.clone())
                    .unwrap_or_default();
                if p.script {
                    version.push_str(if version.is_empty() {
                        "JavaScript"
                    } else {
                        ", JavaScript"
                    });
                }
                let slow = store.stats(&p.name).is_some_and(|s| s.slow());
                (
                    p.name.clone().into(),
                    version.into(),
                    store.state(&p.name),
                    slow,
                )
            })
            .collect();
        let count = rows.len();
        let loaded = store.loaded;
        let folder = store.dir.clone();
        let view = cx.entity();
        let list = uniform_list("plugins", count, move |range, _, cx| {
            let theme = cx.theme().clone();
            range
                .map(|i| {
                    let (name, version, state, slow) = rows[i].clone();
                    let view = view.clone();
                    let toggle = view.clone();
                    let (label, primary) = match &state {
                        PluginState::Enabled => (crate::lang::t("Disable"), false),
                        PluginState::Disabled => (crate::lang::t("Enable"), true),
                        PluginState::Changed => (crate::lang::t("Enable again"), true),
                        PluginState::Invalid(_) => ("", false),
                    };
                    div()
                        .id(("plugin", i))
                        .debug_selector(move || format!("plugin-{i}"))
                        .w_full()
                        .h(crate::theme::row(ROW, cx))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(UI_FONT_SIZE)
                        .when(i == selected, |d| d.bg(theme.bg_elev))
                        .hover(|d| d.bg(theme.bg_elev))
                        .child(div().text_color(theme.fg).child(name))
                        .child(div().text_color(theme.fg_subtle).child(version))
                        .child(div().flex_1())
                        .when(slow, |d| {
                            d.child(Self::badge(crate::lang::t("Slow"), theme.warning, &theme))
                        })
                        .child(match &state {
                            PluginState::Enabled => {
                                Self::badge(crate::lang::t("On"), theme.git_added, &theme)
                            }
                            PluginState::Disabled => {
                                Self::badge(crate::lang::t("Off"), theme.fg_subtle, &theme)
                            }
                            PluginState::Changed => {
                                Self::badge(crate::lang::t("Changed"), theme.warning, &theme)
                            }
                            PluginState::Invalid(_) => {
                                Self::badge(crate::lang::t("Invalid"), theme.error, &theme)
                            }
                        })
                        .when(!label.is_empty(), |d| {
                            d.child(
                                ui::button(
                                    ("plugin-toggle", i),
                                    label,
                                    primary,
                                    &theme,
                                    move |_, _, cx| {
                                        toggle.update(cx, |this, cx| {
                                            this.selected = i;
                                            this.toggle_selected(cx);
                                        })
                                    },
                                )
                                .debug_selector(move || format!("plugin-toggle-{i}"))
                                .h(px(22.)),
                            )
                        })
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.selected = i;
                                cx.notify();
                            })
                        })
                })
                .collect()
        })
        .h(crate::theme::row(ROW, cx) * count.clamp(1, 6) as f32);
        div()
            .key_context("PluginsView")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::toggle))
            .w(px(560.))
            .max_h(px(460.))
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
                    .justify_between()
                    .border_b(theme.shape.border)
                    .border_color(theme.line)
                    .child(
                        div()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg)
                            .child(crate::lang::t("Plugins")),
                    )
                    .child(
                        ui::button("plugins-folder", crate::lang::t("Open folder"), false, &theme, move |_, _, cx| {
                            cx.reveal_path(&folder)
                        })
                        .h(px(22.)),
                    ),
            )
            .map(|d| {
                if count == 0 {
                    d.child(
                        div()
                            .p_3()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg_subtle)
                            .child(if loaded {
                                crate::lang::t("No plugins yet. A plugin is a folder with plugin.json and plugin.wasm or plugin.js in the plugins folder.")
                            } else {
                                crate::lang::t("Reading the plugins folder...")
                            }),
                    )
                } else {
                    d.child(div().flex_none().border_b(theme.shape.border).border_color(theme.line).child(list))
                        .child(self.render_details(&theme, cx))
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_display_keeps_the_approval_key_and_runtime_details() {
        let permission = plugin::Permission::Http("example.com".into());
        crate::lang::set(crate::lang::Language::Russian);
        assert_eq!(
            permission_label(&permission),
            "Отправлять запросы на example.com"
        );
        assert_eq!(permission.name(), "http:example.com");
        assert_eq!(permission.describe(), "Send requests to example.com");
        assert_eq!(
            error_label("The plugin failed: provider detail"),
            "Сбой плагина: provider detail"
        );
        assert_eq!(error_label("provider detail"), "provider detail");
        crate::lang::set(crate::lang::Language::English);
        assert_eq!(permission_label(&permission), permission.describe());
        assert_eq!(
            error_label("The plugin failed: provider detail"),
            "The plugin failed: provider detail"
        );
    }
}
