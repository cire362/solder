//! A project's test tree. Opening the panel runs no project code: discovery
//! and each run are explicit, and only one owns its process at a time.
use crate::{
    extension_store::ExtensionStore,
    extension_views::ExtensionViews,
    test_runner::{self, Cancel, State, Test},
    theme::{ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
    ui,
};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, Subscription,
    UniformListScrollHandle, Window, actions, div, prelude::*, uniform_list,
};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

actions!(project_tests, [Next, Previous, Open, Run, Discover, Stop]);
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", Next, Some("ProjectTests")),
        KeyBinding::new("up", Previous, Some("ProjectTests")),
        KeyBinding::new("enter", Open, Some("ProjectTests")),
        KeyBinding::new("secondary-enter", Run, Some("ProjectTests")),
        KeyBinding::new("secondary-r", Discover, Some("ProjectTests")),
        KeyBinding::new("escape", Stop, Some("ProjectTests")),
    ]);
}
pub enum TestsEvent {
    Open(PathBuf, u32),
}
#[derive(Clone)]
enum Row {
    Group(String),
    Test(usize),
}
struct Result {
    test: Test,
    state: State,
    output: String,
}

pub struct TestsPanel {
    root: PathBuf,
    tests: Vec<Result>,
    rows: Vec<Row>,
    closed: HashSet<String>,
    selected: usize,
    busy: bool,
    cancel: Cancel,
    note: String,
    details: Vec<String>,
    extensions: Entity<ExtensionViews>,
    extension_mode: bool,
    visible: bool,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    _watch: Subscription,
}
impl EventEmitter<TestsEvent> for TestsPanel {}
impl Focusable for TestsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Drop for TestsPanel {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
impl TestsPanel {
    #[cfg(test)]
    pub fn extension_view(&self) -> Entity<ExtensionViews> {
        self.extensions.clone()
    }

    #[cfg(test)]
    pub fn result(&self, name: &str) -> Option<(&State, &str)> {
        self.tests
            .iter()
            .find(|r| r.test.name.ends_with(name))
            .map(|r| (&r.state, r.output.as_str()))
    }
    #[cfg(test)]
    pub fn index(&self, name: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| matches!(row, Row::Test(i) if self.tests[*i].test.name.ends_with(name)))
    }
    pub fn new(root: PathBuf, store: Entity<ExtensionStore>, cx: &mut Context<Self>) -> Self {
        let extensions = cx.new(|cx| ExtensionViews::tests(store, cx));
        let watch = cx.observe(&extensions, |_, _, cx| cx.notify());
        Self {
            root,
            tests: Vec::new(),
            rows: Vec::new(),
            closed: HashSet::new(),
            selected: 0,
            busy: false,
            cancel: Arc::new(AtomicBool::new(false)),
            note: "Discover runs the project's tools. Rust, Go and Python unittest are supported."
                .into(),
            details: Vec::new(),
            extensions,
            extension_mode: false,
            visible: false,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            _watch: watch,
        }
    }
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        self.extensions.update(cx, |p, cx| {
            p.set_visible(visible && self.extension_mode, cx)
        });
    }
    fn rebuild(&mut self) {
        self.rows.clear();
        let mut group = None;
        for (i, result) in self.tests.iter().enumerate() {
            if group != Some(&result.test.group) {
                self.rows.push(Row::Group(result.test.group.clone()));
                group = Some(&result.test.group);
            }
            if !self.closed.contains(&result.test.group) {
                self.rows.push(Row::Test(i));
            }
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.show_details();
    }
    fn show_details(&mut self) {
        self.details = match self.rows.get(self.selected) {
            Some(Row::Test(i)) => self.tests[*i]
                .output
                .lines()
                .take(2000)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
    }
    pub fn discover(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.note = "Discovering tests…".into();
        let root = self.root.clone();
        let cancel = self.cancel.clone();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { test_runner::discover(&root, &cancel) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                this.note = if this.cancel.load(Ordering::Relaxed) {
                    "Discovery cancelled".into()
                } else if !found.errors.is_empty() {
                    found.errors.join("\n").chars().take(2000).collect()
                } else {
                    format!("{} tests", found.tests.len())
                };
                this.tests = found
                    .tests
                    .into_iter()
                    .map(|test| Result {
                        test,
                        state: State::Ready,
                        output: String::new(),
                    })
                    .collect();
                this.rebuild();
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
    fn discover_action(&mut self, _: &Discover, _: &mut Window, cx: &mut Context<Self>) {
        self.discover(cx);
    }
    fn stop(&mut self, _: &Stop, _: &mut Window, _: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        self.selected = i.min(self.rows.len().saturating_sub(1));
        self.show_details();
        self.scroll
            .scroll_to_item(self.selected, gpui::ScrollStrategy::Top);
        cx.notify();
    }
    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected + 1, cx);
    }
    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected.saturating_sub(1), cx);
    }
    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        self.activate(self.selected, cx);
    }
    fn activate(&mut self, i: usize, cx: &mut Context<Self>) {
        self.select(i, cx);
        match self.rows.get(i).cloned() {
            Some(Row::Group(group)) => {
                if !self.closed.remove(&group) {
                    self.closed.insert(group);
                }
                self.rebuild();
                cx.notify();
            }
            Some(Row::Test(i)) => {
                if let Some(path) = self.tests[i].test.path.clone() {
                    cx.emit(TestsEvent::Open(path, self.tests[i].test.line));
                }
            }
            None => {}
        }
    }
    fn run_selected(&mut self, _: &Run, _: &mut Window, cx: &mut Context<Self>) {
        let indices = match self.rows.get(self.selected) {
            Some(Row::Test(i)) => vec![*i],
            Some(Row::Group(group)) => self
                .tests
                .iter()
                .enumerate()
                .filter(|(_, r)| &r.test.group == group)
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        };
        self.run(indices, cx);
    }
    fn run(&mut self, indices: Vec<usize>, cx: &mut Context<Self>) {
        if self.busy || indices.is_empty() {
            return;
        }
        self.busy = true;
        self.cancel = Arc::new(AtomicBool::new(false));
        let root = self.root.clone();
        let cancel = self.cancel.clone();
        let tests: Vec<_> = indices
            .iter()
            .map(|i| (*i, self.tests[*i].test.clone()))
            .collect();
        for i in indices {
            self.tests[i].state = State::Ready;
            self.tests[i].output.clear();
        }
        cx.spawn(async move |this, cx| {
            for (i, test) in tests {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if this
                    .update(cx, |this, cx| {
                        this.tests[i].state = State::Running;
                        this.note = format!("Running {}", test.name);
                        this.show_details();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                let (root, cancel) = (root.clone(), cancel.clone());
                let result = cx
                    .background_executor()
                    .spawn(async move { test_runner::run(&root, &test, &cancel) })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.tests[i].state = result.state;
                        this.tests[i].output = result.output;
                        this.show_details();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            this.update(cx, |this, cx| {
                this.busy = false;
                this.note = if this.cancel.load(Ordering::Relaxed) {
                    "Run cancelled".into()
                } else {
                    "Run finished".into()
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}
impl Render for TestsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context("ProjectTests")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::run_selected))
            .on_action(cx.listener(Self::discover_action))
            .on_action(cx.listener(Self::stop))
            .child(
                div()
                    .p_1()
                    .flex()
                    .gap_1()
                    .flex_wrap()
                    .child(
                        ui::button(
                            "tests-project",
                            "Project",
                            !self.extension_mode,
                            &theme,
                            cx.listener(|this, _, _, cx| {
                                this.extension_mode = false;
                                this.set_visible(this.visible, cx);
                                cx.notify();
                            }),
                        )
                        .debug_selector(|| "tests-project".into()),
                    )
                    .child(
                        ui::button(
                            "tests-extensions",
                            "Extensions",
                            self.extension_mode,
                            &theme,
                            cx.listener(|this, _, window, cx| {
                                this.extension_mode = true;
                                this.set_visible(this.visible, cx);
                                window.focus(&this.extensions.focus_handle(cx));
                                cx.notify();
                            }),
                        )
                        .debug_selector(|| "tests-extensions".into()),
                    ),
            )
            .when(self.extension_mode, |d| d.child(self.extensions.clone()))
            .when(!self.extension_mode, |d| {
                d.child(
                    div()
                        .p_1()
                        .flex()
                        .gap_1()
                        .flex_wrap()
                        .when(!self.busy, |d| {
                            d.child(
                                ui::button(
                                    "tests-discover",
                                    "Discover",
                                    false,
                                    &theme,
                                    cx.listener(|this, _, _, cx| this.discover(cx)),
                                )
                                .debug_selector(|| "tests-discover".into()),
                            )
                            .child(
                                ui::button(
                                    "tests-run",
                                    "Run",
                                    true,
                                    &theme,
                                    cx.listener(|this, _, window, cx| {
                                        this.run_selected(&Run, window, cx)
                                    }),
                                )
                                .debug_selector(|| "tests-run".into()),
                            )
                            .child(
                                ui::button(
                                    "tests-all",
                                    "Run all",
                                    false,
                                    &theme,
                                    cx.listener(|this, _, _, cx| {
                                        this.run((0..this.tests.len()).collect(), cx)
                                    }),
                                )
                                .debug_selector(|| "tests-all".into()),
                            )
                        })
                        .when(self.busy, |d| {
                            d.child(
                                ui::button(
                                    "tests-stop",
                                    "Stop",
                                    false,
                                    &theme,
                                    cx.listener(|this, _, window, cx| this.stop(&Stop, window, cx)),
                                )
                                .debug_selector(|| "tests-stop".into()),
                            )
                        }),
                )
                .child(
                    div()
                        .p_2()
                        .text_size(UI_FONT_SMALL)
                        .text_color(theme.fg_muted)
                        .child(self.note.clone()),
                )
                .child(
                    uniform_list(
                        "project-tests",
                        self.rows.len(),
                        cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                            let theme = cx.theme().clone();
                            range
                                .map(|i| {
                                    let (name, state) = match &this.rows[i] {
                                        Row::Group(group) => (
                                            format!(
                                                "{} {group}",
                                                if this.closed.contains(group) {
                                                    "▸"
                                                } else {
                                                    "▾"
                                                }
                                            ),
                                            State::Ready,
                                        ),
                                        Row::Test(at) => (
                                            format!("  {}", this.tests[*at].test.name),
                                            this.tests[*at].state.clone(),
                                        ),
                                    };
                                    div()
                                        .id(("project-test", i))
                                        .debug_selector(move || format!("project-test-{i}"))
                                        .h(crate::theme::row(gpui::px(28.), cx))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_size(UI_FONT_SIZE)
                                        .when(i == this.selected, |d| d.bg(theme.accent_soft))
                                        .hover(|d| d.bg(theme.bg_elev))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(theme.fg)
                                                .child(name),
                                        )
                                        .child(
                                            div()
                                                .text_size(UI_FONT_SMALL)
                                                .text_color(match state {
                                                    State::Failed => theme.error,
                                                    State::Passed => theme.git_added,
                                                    _ => theme.fg_muted,
                                                })
                                                .child(state.label()),
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| this.activate(i, cx)),
                                        )
                                })
                                .collect()
                        }),
                    )
                    .flex_1()
                    .track_scroll(self.scroll.clone()),
                )
                .when(!self.details.is_empty(), |d| {
                    d.child(
                        uniform_list(
                            "test-output",
                            self.details.len(),
                            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                let theme = cx.theme().clone();
                                range
                                    .map(|i| {
                                        div()
                                            .id(("test-output-line", i))
                                            .w_full()
                                            .truncate()
                                            .tooltip({
                                                let line = this.details[i].clone();
                                                move |_, cx| {
                                                    cx.new(|_| ui::Tooltip(line.clone())).into()
                                                }
                                            })
                                            .h(crate::theme::row(gpui::px(24.), cx))
                                            .px_2()
                                            .text_size(UI_FONT_SMALL)
                                            .font_family(crate::theme::CODE_FONT)
                                            .text_color(theme.fg_muted)
                                            .child(this.details[i].clone())
                                    })
                                    .collect()
                            }),
                        )
                        .flex_1(),
                    )
                })
            })
    }
}
