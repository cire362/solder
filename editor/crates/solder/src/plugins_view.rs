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
            .rounded(px(6.))
            .bg(theme.bg_sunken)
            .text_size(px(11.))
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
            Err(error) => details = details.child(line(error.clone().into(), theme.error)),
            Ok(manifest) => {
                if !manifest.description.is_empty() {
                    details = details.child(line(manifest.description.clone().into(), theme.fg));
                }
                details = details.child(
                    div()
                        .pt_1()
                        .text_size(px(10.5))
                        .text_color(theme.fg_subtle)
                        .child(match state {
                            PluginState::Enabled => "ALLOWED",
                            _ => "ENABLING IT ALLOWS IT TO",
                        }),
                );
                if manifest.permissions.is_empty() {
                    details = details.child(line(
                        "Nothing outside its own memory".into(),
                        theme.fg_muted,
                    ));
                }
                for permission in &manifest.permissions {
                    details = details.child(line(permission.describe().into(), theme.fg_muted));
                }
            }
        }
        match &state {
            PluginState::Changed => {
                details = details.child(line(
                    "Changed since it was enabled. Check the list and enable it again.".into(),
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
                details = details.child(line(error.clone().into(), theme.error));
            }
            if stats.events > 0 {
                details = details.child(line(
                    format!(
                        "{} events. Last took {:.1} ms, the slowest {:.1} ms.",
                        stats.events, stats.last_ms, stats.slowest_ms
                    )
                    .into(),
                    theme.fg_subtle,
                ));
            }
            if stats.over_budget > 0 || stats.stopped > 0 {
                details = details.child(line(
                    format!(
                        "Over the typing budget {} times, stopped at the limit {} times.",
                        stats.over_budget, stats.stopped
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
                    format!("{} failed. Last: {failure}", stats.failures).into(),
                    theme.error,
                ));
            }
            for entry in stats.log.iter().rev().take(5).rev() {
                details = details.child(
                    div()
                        .font_family(crate::theme::CODE_FONT)
                        .text_size(px(11.5))
                        .text_color(theme.fg_subtle)
                        .child(entry.clone()),
                );
            }
        }
        details.into_any_element()
    }
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
                let version = p
                    .manifest
                    .as_ref()
                    .map(|m| m.version.clone())
                    .unwrap_or_default();
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
                        PluginState::Enabled => ("Disable", false),
                        PluginState::Disabled => ("Enable", true),
                        PluginState::Changed => ("Enable again", true),
                        PluginState::Invalid(_) => ("", false),
                    };
                    div()
                        .id(("plugin", i))
                        .debug_selector(move || format!("plugin-{i}"))
                        .w_full()
                        .h(ROW)
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
                            d.child(Self::badge("Slow", theme.warning, &theme))
                        })
                        .child(match &state {
                            PluginState::Enabled => Self::badge("On", theme.git_added, &theme),
                            PluginState::Disabled => Self::badge("Off", theme.fg_subtle, &theme),
                            PluginState::Changed => Self::badge("Changed", theme.warning, &theme),
                            PluginState::Invalid(_) => Self::badge("Invalid", theme.error, &theme),
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
        .h(ROW * count.clamp(1, 6) as f32);
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
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.line)
                    .child(
                        div()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg)
                            .child("Plugins"),
                    )
                    .child(
                        ui::button("plugins-folder", "Open folder", false, &theme, move |_, _, cx| {
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
                                "No plugins yet. A plugin is a folder with plugin.json and plugin.wasm in the plugins folder."
                            } else {
                                "Reading the plugins folder..."
                            }),
                    )
                } else {
                    d.child(div().flex_none().border_b_1().border_color(theme.line).child(list))
                        .child(self.render_details(&theme, cx))
                }
            })
    }
}
