//! The Debug tab of the bottom dock: what to run, the controls, the call
//! stack and variables of a pause, and the console, where expressions run
//! in the paused frame.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, KeyBinding, SharedString, Window,
    actions, div, prelude::*, px, uniform_list,
};

use crate::{
    debug::{DebugEvent, DebugStore, State},
    debug_launch::{self, LaunchConfig},
    debug_timeline::Kind,
    editor::Editor,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(debug_panel, [Evaluate]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("enter", Evaluate, Some("DebugConsole"))]);
}

const ROW: gpui::Pixels = px(22.);

/// One row of the variables tree.
#[derive(Clone)]
struct VarRow {
    depth: usize,
    name: String,
    value: String,
    reference: i64,
    open: bool,
}

pub struct DebugPanel {
    store: Entity<DebugStore>,
    root: PathBuf,
    pub configs: Vec<LaunchConfig>,
    pub selected: usize,
    picking: bool,
    timeline_open: bool,
    input: Entity<Editor>,
    focus: FocusHandle,
}

impl DebugPanel {
    pub fn new(store: Entity<DebugStore>, root: PathBuf, cx: &mut Context<Self>) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self {
            store,
            root,
            configs: Vec::new(),
            selected: 0,
            picking: false,
            timeline_open: false,
            input: cx.new(|cx| Editor::single_line("Evaluate an expression", cx)),
            focus: cx.focus_handle(),
        }
    }

    #[cfg(test)]
    pub fn input(&self) -> Entity<Editor> {
        self.input.clone()
    }

    /// Reads what can be debugged, keeping the choice when it still exists;
    /// with `start`, then runs the chosen one.
    pub fn refresh_configs(&mut self, file: Option<PathBuf>, start: bool, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let current = self.configs.get(self.selected).map(|c| c.name.clone());
        let found = cx
            .background_executor()
            .spawn(async move { debug_launch::detect(&root, file.as_deref()) });
        cx.spawn(async move |this, cx| {
            let configs = found.await;
            this.update(cx, |this, cx| {
                this.selected = current
                    .and_then(|name| configs.iter().position(|c| c.name == name))
                    .unwrap_or(0);
                this.configs = configs;
                if start {
                    this.start_or_continue(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn toggle_picker(&mut self, cx: &mut Context<Self>) {
        self.picking = !self.picking;
        cx.notify();
    }

    /// F5: start the chosen configuration, or continue a pause.
    pub fn start_or_continue(&mut self, cx: &mut Context<Self>) {
        let state = self.store.read(cx).state.clone();
        match state {
            State::Paused => self.store.update(cx, |s, cx| s.resume(cx)),
            State::Running | State::Starting(_) => {}
            State::Idle | State::Failed(_) => {
                let Some(config) = self.configs.get(self.selected).cloned() else {
                    return;
                };
                let root = self.root.clone();
                self.store.update(cx, |s, cx| s.start(config, root, cx));
            }
        }
    }

    fn evaluate(&mut self, _: &Evaluate, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text(cx);
        if text.trim().is_empty() {
            return;
        }
        self.input.update(cx, |e, cx| e.set_text("", false, cx));
        self.store.update(cx, |s, cx| s.evaluate(text, cx));
    }

    fn var_rows(&self, cx: &App) -> Vec<VarRow> {
        let store = self.store.read(cx);
        let mut rows = Vec::new();
        fn walk(store: &DebugStore, reference: i64, depth: usize, rows: &mut Vec<VarRow>) {
            if depth > 12 {
                return;
            }
            for v in store.children.get(&reference).into_iter().flatten() {
                let open = store.expanded.contains(&v.reference);
                rows.push(VarRow {
                    depth,
                    name: v.name.clone(),
                    value: v.value.clone(),
                    reference: v.reference,
                    open,
                });
                if open {
                    walk(store, v.reference, depth + 1, rows);
                }
            }
        }
        for (name, reference) in &store.scopes {
            let open = store.expanded.contains(reference);
            rows.push(VarRow {
                depth: 0,
                name: name.clone(),
                value: String::new(),
                reference: *reference,
                open,
            });
            if open {
                walk(store, *reference, 1, &mut rows);
            }
        }
        rows
    }

    fn control(
        &self,
        id: &'static str,
        label: &'static str,
        primary: bool,
        theme: &Theme,
        f: impl Fn(&mut DebugStore, &mut Context<DebugStore>) + 'static,
    ) -> AnyElement {
        let store = self.store.clone();
        ui::button(id, label, primary, theme, move |_, _, cx| {
            store.update(cx, |s, cx| f(s, cx))
        })
        .debug_selector(move || id.into())
        .into_any_element()
    }

    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let state = store.state.clone();
        let small = UI_FONT_SIZE - px(1.);
        let mut bar = div()
            .flex_none()
            .h(px(36.))
            .px_2()
            .flex()
            .items_center()
            .gap_1p5()
            .border_b_1()
            .border_color(theme.line);
        let config = self.configs.get(self.selected).map(|c| c.name.clone());
        match &state {
            State::Idle | State::Failed(_) => {
                bar = bar
                    .child(
                        div()
                            .id("debug-config")
                            .debug_selector(|| "debug-config".into())
                            .max_w(px(320.))
                            .px_2()
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(theme.line)
                            .hover(|d| d.bg(theme.bg_elev))
                            .text_size(UI_FONT_SIZE)
                            .text_color(if config.is_some() {
                                theme.fg
                            } else {
                                theme.fg_subtle
                            })
                            .child(
                                div().truncate().child(
                                    config
                                        .clone()
                                        .unwrap_or_else(|| "Nothing to debug here".into()),
                                ),
                            )
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(crate::workspace::DebugPick), cx)
                            }),
                    )
                    .when(config.is_some(), |d| {
                        d.child(
                            ui::button("debug-start", "Start", true, theme, |_, window, cx| {
                                window.dispatch_action(Box::new(crate::workspace::DebugStart), cx)
                            })
                            .debug_selector(|| "debug-start".into()),
                        )
                    });
            }
            State::Starting(_) | State::Running => {
                bar = bar
                    .child(self.control("debug-pause", "Pause", false, theme, |s, cx| s.pause(cx)))
                    .child(self.control("debug-stop", "Stop", false, theme, |s, cx| s.stop(cx)));
            }
            State::Paused => {
                bar = bar
                    .child(
                        self.control("debug-continue", "Continue", true, theme, |s, cx| {
                            s.resume(cx)
                        }),
                    )
                    .child(
                        self.control("debug-over", "Over", false, theme, |s, cx| s.step_over(cx)),
                    )
                    .child(self.control("debug-in", "Into", false, theme, |s, cx| s.step_in(cx)))
                    .child(self.control("debug-out", "Out", false, theme, |s, cx| s.step_out(cx)))
                    .child(self.control("debug-stop", "Stop", false, theme, |s, cx| s.stop(cx)));
            }
        }
        let status: (String, gpui::Hsla) = match &state {
            State::Idle => (
                "F5 starts; click a line number for a breakpoint".into(),
                theme.fg_subtle,
            ),
            State::Starting(s) => (s.to_string(), theme.fg_subtle),
            State::Running => (
                format!(
                    "Running {}",
                    store.config.as_ref().map(|c| c.name.as_str()).unwrap_or("")
                ),
                theme.git_added,
            ),
            State::Paused => (
                store
                    .paused
                    .as_ref()
                    .map(|p| match store.session_name(p.session) {
                        Some(name) => format!("Paused on {} in {name}", p.reason),
                        None => format!("Paused on {}", p.reason),
                    })
                    .unwrap_or_default(),
                theme.warning,
            ),
            State::Failed(e) => (e.to_string(), theme.error),
        };
        bar.child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(small)
                .text_color(status.1)
                .child(status.0),
        )
        .into_any_element()
    }

    fn render_picker(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.picking {
            return None;
        }
        let mut menu = div()
            .mx_2()
            .mt_1()
            .p_1()
            .rounded(px(8.))
            .border_1()
            .border_color(theme.line)
            .bg(theme.bg_elev)
            .flex()
            .flex_col();
        if self.configs.is_empty() {
            menu = menu.child(
                div()
                    .p_2()
                    .text_size(UI_FONT_SIZE - px(1.))
                    .text_color(theme.fg_subtle)
                    .child("Open a .js or .ts file, or add scripts to package.json."),
            );
        }
        for (i, c) in self.configs.iter().enumerate() {
            menu = menu.child(
                div()
                    .id(("debug-config-item", i))
                    .debug_selector(move || format!("debug-config-{i}"))
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .text_size(UI_FONT_SIZE)
                    .text_color(if i == self.selected {
                        theme.accent
                    } else {
                        theme.fg
                    })
                    .hover(|d| d.bg(theme.accent_soft))
                    .child(c.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = i;
                        this.picking = false;
                        cx.notify();
                    })),
            );
        }
        Some(menu.into_any_element())
    }

    /// Console and Timeline, over the right half.
    fn render_tabs(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let count = self.store.read(cx).timeline.len();
        let tab = |id: &'static str, label: SharedString, active: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px_1()
                .text_size(px(10.5))
                .text_color(if active { theme.fg } else { theme.fg_subtle })
                .hover(|d| d.text_color(theme.fg))
                .child(label)
        };
        div()
            .flex_none()
            .px_1()
            .pt_1p5()
            .pb_0p5()
            .flex()
            .gap_2()
            .child(
                tab("debug-console-tab", "CONSOLE".into(), !self.timeline_open).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.timeline_open = false;
                        cx.notify();
                    }),
                ),
            )
            .child(
                tab(
                    "debug-timeline-tab",
                    if count == 0 {
                        "TIMELINE".into()
                    } else {
                        format!("TIMELINE {count}").into()
                    },
                    self.timeline_open,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.timeline_open = true;
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    /// Served and sent requests and queries in the order they began; work
    /// done for a served request sits under it. A row opens its call site.
    fn render_timeline(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        if store.timeline.is_empty() {
            return div()
                .flex_1()
                .px_2()
                .pt_1()
                .text_size(px(12.))
                .text_color(theme.fg_subtle)
                .child("Requests and SQL queries of a Node run show up here.")
                .into_any_element();
        }
        let entries = store.timeline.clone();
        let count = entries.len();
        let store = self.store.clone();
        uniform_list("debug-timeline", count, move |range, _, cx| {
            let theme = cx.theme().clone();
            range
                .map(|i| {
                    let e = &entries[i];
                    let (badge, badge_color) = match e.kind {
                        Kind::In => ("IN", theme.accent),
                        Kind::Out => ("OUT", theme.syntax.function),
                        Kind::Sql => ("SQL", theme.syntax.string),
                    };
                    let result: SharedString = match (&e.error, e.status, e.rows) {
                        (Some(_), _, _) => "failed".into(),
                        (None, Some(status), _) => status.to_string().into(),
                        (None, None, Some(1)) => "1 row".into(),
                        (None, None, Some(rows)) => format!("{rows} rows").into(),
                        _ => "".into(),
                    };
                    let place: SharedString = match (&e.path, e.line) {
                        (Some(path), Some(line)) => format!(
                            "{}:{line}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        )
                        .into(),
                        _ => "".into(),
                    };
                    let target = e.path.clone().zip(e.line);
                    let store = store.clone();
                    div()
                        .id(("debug-timeline-row", i))
                        .debug_selector(move || format!("debug-timeline-{i}"))
                        .w_full()
                        .h(ROW)
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(12.))
                        .hover(|d| d.bg(theme.bg_elev))
                        .when(e.parent.is_some(), |d| d.pl(px(22.)))
                        .child(
                            div()
                                .w(px(28.))
                                .flex_none()
                                .text_size(px(10.5))
                                .text_color(badge_color)
                                .child(badge),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(crate::theme::CODE_FONT)
                                .text_color(theme.fg)
                                .child(e.label.clone()),
                        )
                        .child(
                            div()
                                .w(px(56.))
                                .flex_none()
                                .text_color(if e.failed() {
                                    theme.error
                                } else {
                                    theme.fg_muted
                                })
                                .child(result),
                        )
                        .child(
                            div()
                                .w(px(64.))
                                .flex_none()
                                .flex()
                                .justify_end()
                                .text_color(if e.ms >= 1000. {
                                    theme.warning
                                } else {
                                    theme.fg_muted
                                })
                                .child(duration(e.ms)),
                        )
                        .child(
                            div()
                                .w(px(120.))
                                .flex_none()
                                .truncate()
                                .text_color(theme.fg_subtle)
                                .child(place),
                        )
                        .when_some(target, |d, (path, line)| {
                            d.cursor_pointer().on_click(move |_, _, cx| {
                                let (path, line) = (path.clone(), line);
                                store.update(cx, |_, cx| cx.emit(DebugEvent::Reveal(path, line)))
                            })
                        })
                })
                .collect()
        })
        .flex_1()
        .into_any_element()
    }

    fn header(label: &'static str, theme: &Theme) -> AnyElement {
        div()
            .flex_none()
            .px_2()
            .pt_1p5()
            .pb_0p5()
            .text_size(px(10.5))
            .text_color(theme.fg_subtle)
            .child(label)
            .into_any_element()
    }
}

/// `12 ms`, `1.4 s`.
fn duration(ms: f64) -> SharedString {
    if ms < 1000. {
        format!("{} ms", ms.round() as u64).into()
    } else {
        format!("{:.1} s", ms / 1000.).into()
    }
}

impl Focusable for DebugPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DebugPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let toolbar = self.render_toolbar(&theme, cx);
        let picker = self.render_picker(&theme, cx);
        let store = self.store.read(cx);
        let frames: Vec<(String, String, bool, bool)> = store
            .paused
            .as_ref()
            .map(|p| {
                p.frames
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let place = f
                            .path
                            .as_ref()
                            .map(|path| {
                                let shown = path.strip_prefix(&self.root).unwrap_or(path);
                                format!("{}:{}", shown.display(), f.line)
                            })
                            .unwrap_or_default();
                        (f.name.clone(), place, i == p.selected, f.subtle)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let console: Vec<(String, String)> = store
            .console
            .iter()
            .map(|l| (l.category.clone(), l.text.clone()))
            .collect();
        let vars = self.var_rows(cx);
        let focused = self.input.focus_handle(cx).is_focused(window);
        let store_entity = self.store.clone();
        let frame_rows = frames.len();
        let var_count = vars.len();
        let console_count = console.len();
        let timeline = self.timeline_open.then(|| self.render_timeline(&theme, cx));
        div()
            .key_context("DebugPanel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .child(toolbar)
            .children(picker)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .w(px(280.))
                            .flex_none()
                            .h_full()
                            .flex()
                            .flex_col()
                            .border_r_1()
                            .border_color(theme.line)
                            .child(Self::header("CALL STACK", &theme))
                            .child(
                                uniform_list("debug-frames", frame_rows, {
                                    let store = store_entity.clone();
                                    move |range, _, cx| {
                                        let theme = cx.theme().clone();
                                        range
                                            .map(|i| {
                                                let (name, place, selected, subtle) =
                                                    frames[i].clone();
                                                let store = store.clone();
                                                div()
                                                    .id(("debug-frame", i))
                                                    .debug_selector(move || {
                                                        format!("debug-frame-{i}")
                                                    })
                                                    .h(ROW)
                                                    .px_2()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .text_size(px(12.))
                                                    .when(selected, |d| d.bg(theme.accent_soft))
                                                    .hover(|d| d.bg(theme.bg_elev))
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_color(if subtle {
                                                                theme.fg_subtle
                                                            } else {
                                                                theme.fg
                                                            })
                                                            .child(name),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .truncate()
                                                            .text_color(theme.fg_subtle)
                                                            .child(place),
                                                    )
                                                    .on_click(move |_, _, cx| {
                                                        store.update(cx, |s, cx| {
                                                            s.select_frame(i, cx)
                                                        })
                                                    })
                                            })
                                            .collect()
                                    }
                                })
                                .flex_1(),
                            ),
                    )
                    // Stack, variables and console side by side: the dock is
                    // short, so stacking them left the variables a row or two.
                    .child(
                        div()
                            .w(px(340.))
                            .flex_none()
                            .h_full()
                            .flex()
                            .flex_col()
                            .border_r_1()
                            .border_color(theme.line)
                            .child(Self::header("VARIABLES", &theme))
                            .child(
                                uniform_list("debug-vars", var_count, {
                                    let store = store_entity.clone();
                                    move |range, _, cx| {
                                        let theme = cx.theme().clone();
                                        range
                                            .map(|i| {
                                                let v = vars[i].clone();
                                                let store = store.clone();
                                                let reference = v.reference;
                                                div()
                                                    .id(("debug-var", i))
                                                    .debug_selector(move || {
                                                        format!("debug-var-{i}")
                                                    })
                                                    .h(ROW)
                                                    .pl(px(8. + 14. * v.depth as f32))
                                                    .pr_2()
                                                    .flex()
                                                    .items_center()
                                                    .gap_1()
                                                    .text_size(px(12.))
                                                    .font_family(crate::theme::CODE_FONT)
                                                    .hover(|d| d.bg(theme.bg_elev))
                                                    .child(
                                                        div()
                                                            .w(px(10.))
                                                            .flex_none()
                                                            .text_color(theme.fg_subtle)
                                                            .child(
                                                                match (reference != 0, v.open) {
                                                                    (false, _) => "",
                                                                    (true, true) => "▾",
                                                                    (true, false) => "▸",
                                                                },
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex_none()
                                                            .text_color(if v.depth == 0 {
                                                                theme.fg_muted
                                                            } else {
                                                                theme.syntax.function
                                                            })
                                                            .child(v.name),
                                                    )
                                                    .when(!v.value.is_empty(), |d| {
                                                        d.child(
                                                            div()
                                                                .text_color(theme.fg_subtle)
                                                                .child("="),
                                                        )
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .truncate()
                                                                .text_color(theme.fg)
                                                                .child(v.value),
                                                        )
                                                    })
                                                    .on_click(move |_, _, cx| {
                                                        store.update(cx, |s, cx| {
                                                            s.toggle_expanded(reference, cx)
                                                        })
                                                    })
                                            })
                                            .collect()
                                    }
                                })
                                .flex_1(),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .child(self.render_tabs(&theme, cx))
                            .children(timeline)
                            .when(!self.timeline_open, |d| {
                                d.child(
                                    uniform_list(
                                        "debug-console",
                                        console_count,
                                        move |range, _, cx| {
                                            let theme = cx.theme().clone();
                                            range
                                                .map(|i| {
                                                    let (category, text) = &console[i];
                                                    let color = match category.as_str() {
                                                        "stderr" => theme.error,
                                                        "input" => theme.fg_subtle,
                                                        "result" => theme.accent,
                                                        "console" => theme.fg_muted,
                                                        _ => theme.fg,
                                                    };
                                                    let shown: SharedString = if category == "input"
                                                    {
                                                        format!("› {text}").into()
                                                    } else {
                                                        text.clone().into()
                                                    };
                                                    div()
                                                        .h(ROW)
                                                        .px_2()
                                                        .flex()
                                                        .items_center()
                                                        .font_family(crate::theme::CODE_FONT)
                                                        .text_size(px(12.))
                                                        .text_color(color)
                                                        .child(div().truncate().child(shown))
                                                })
                                                .collect()
                                        },
                                    )
                                    .flex_1(),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .p_1p5()
                                        .border_t_1()
                                        .border_color(theme.line)
                                        .flex()
                                        .key_context("DebugConsole")
                                        .on_action(cx.listener(Self::evaluate))
                                        .child(ui::text_field(self.input.clone(), focused, &theme)),
                                )
                            }),
                    ),
            )
    }
}
