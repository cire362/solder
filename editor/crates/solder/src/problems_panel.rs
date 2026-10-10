//! The Problems panel: everything the language servers report of the
//! project's files, by file. A file need not be open to be in it: a server
//! says what it finds in files it read itself.

use std::{
    collections::HashSet,
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    Subscription, UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::{
    document::Severity,
    lsp_store::{LspStore, LspStoreEvent, Problem},
    theme::{ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
    workspace::Jump,
};

actions!(problems, [Next, Previous, Open, Expand, Collapse]);

pub fn bind_keys(cx: &mut App) {
    let context = Some("Problems");
    cx.bind_keys([
        KeyBinding::new("down", Next, context),
        KeyBinding::new("up", Previous, context),
        KeyBinding::new("enter", Open, context),
        KeyBinding::new("right", Expand, context),
        KeyBinding::new("left", Collapse, context),
    ]);
}

pub enum ProblemsEvent {
    /// Open a file at the place of a problem in it.
    Open(PathBuf, Jump),
}

#[derive(Clone)]
enum Row {
    /// A file with problems: its name from the project's folder, and how
    /// many of each kind it has.
    File {
        path: PathBuf,
        name: String,
        errors: usize,
        warnings: usize,
        others: usize,
    },
    Problem(PathBuf, Problem),
}

pub struct ProblemsPanel {
    store: Option<Entity<LspStore>>,
    root: PathBuf,
    rows: Vec<Row>,
    /// The files whose problems are folded away.
    closed: HashSet<PathBuf>,
    /// How many errors, warnings and files there are in all.
    totals: (usize, usize, usize),
    selected: usize,
    /// Read only while it is on screen: a project being built reports
    /// problems faster than a list nobody looks at needs them.
    visible: bool,
    stale: bool,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _events: Option<Subscription>,
}

impl EventEmitter<ProblemsEvent> for ProblemsPanel {}

impl Focusable for ProblemsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ProblemsPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let store = LspStore::global(cx);
        let events = store.as_ref().map(|store| {
            cx.subscribe(store, |this, _, event, cx| {
                if matches!(event, LspStoreEvent::ProblemsChanged) {
                    this.changed(cx);
                }
            })
        });
        Self {
            store,
            root,
            rows: Vec::new(),
            closed: HashSet::new(),
            totals: (0, 0, 0),
            selected: 0,
            visible: false,
            stale: true,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _events: events,
        }
    }

    /// Whether its tab is in front. Coming into view, it reads what was
    /// reported while it was not.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if visible && self.stale {
            self.rebuild(cx);
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        match self.visible {
            true => self.rebuild(cx),
            false => self.stale = true,
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.stale = false;
        let mut rows = Vec::new();
        let mut totals = (0, 0, 0);
        if let Some(store) = &self.store {
            for (path, problems) in store.read(cx).problems() {
                let count = |severity: Severity| {
                    let of = problems
                        .iter()
                        .filter(|problem| problem.severity == severity);
                    of.count()
                };
                let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
                totals = (totals.0 + errors, totals.1 + warnings, totals.2 + 1);
                let name = path.strip_prefix(&self.root).unwrap_or(path);
                rows.push(Row::File {
                    path: path.to_path_buf(),
                    name: name.display().to_string(),
                    errors,
                    warnings,
                    others: problems.len() - errors - warnings,
                });
                if !self.closed.contains(path) {
                    let of_file = problems.into_iter().cloned();
                    rows.extend(of_file.map(|problem| Row::Problem(path.to_path_buf(), problem)));
                }
            }
        }
        self.rows = rows;
        self.totals = totals;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        cx.notify();
    }

    #[cfg(test)]
    pub fn shown(&self) -> Vec<String> {
        let line = |row: &Row| match row {
            Row::File {
                name,
                errors,
                warnings,
                others,
                ..
            } => format!("{name}: {errors} errors, {warnings} warnings, {others} others"),
            Row::Problem(_, problem) => format!(
                "  {}:{} {}",
                problem.range.start.line + 1,
                problem.range.start.character + 1,
                problem.message
            ),
        };
        self.rows.iter().map(line).collect()
    }

    fn select(&mut self, at: usize, cx: &mut Context<Self>) {
        if at < self.rows.len() {
            self.selected = at;
            self.scroll.scroll_to_item(at, gpui::ScrollStrategy::Top);
            cx.notify();
        }
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected + 1, cx);
    }

    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected.saturating_sub(1), cx);
    }

    /// A problem is gone to; a file's problems are folded away or brought
    /// back.
    fn activate(&mut self, at: usize, cx: &mut Context<Self>) {
        match self.rows.get(at).cloned() {
            Some(Row::Problem(path, problem)) => {
                let jump = Jump::Lsp {
                    range: problem.range,
                    encoding: problem.encoding,
                };
                cx.emit(ProblemsEvent::Open(path, jump));
            }
            Some(Row::File { path, .. }) => {
                if !self.closed.remove(&path) {
                    self.closed.insert(path);
                }
                self.rebuild(cx);
            }
            None => {}
        }
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        self.activate(self.selected, cx);
    }

    fn fold(&mut self, close: bool, cx: &mut Context<Self>) {
        let path: &Path = match self.rows.get(self.selected) {
            Some(Row::File { path, .. }) | Some(Row::Problem(path, _)) => path,
            None => return,
        };
        let path = path.to_path_buf();
        let changed = match close {
            true => self.closed.insert(path.clone()),
            false => self.closed.remove(&path),
        };
        if changed {
            // The file's own row stays under the keys.
            let file = |row: &Row| matches!(row, Row::File { path: of, .. } if *of == path);
            self.rebuild(cx);
            self.selected = self.rows.iter().position(file).unwrap_or(self.selected);
        }
    }

    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        self.fold(false, cx);
    }

    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        self.fold(true, cx);
    }

    fn list(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let focused = self.focus.contains_focused(window, cx);
        range
            .map(|at| {
                let row = div()
                    .id(("problem-row", at))
                    .debug_selector(move || format!("problem-{at}"))
                    .w_full()
                    .h(crate::theme::row(px(24.), cx))
                    .pr_2()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(UI_FONT_SIZE)
                    .when(self.selected == at, |d| {
                        d.bg(if focused {
                            theme.accent_soft
                        } else {
                            theme.bg_elev
                        })
                    })
                    .hover(|d| d.bg(theme.bg_elev))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = at;
                        this.activate(at, cx);
                    }));
                match &self.rows[at] {
                    Row::File {
                        path,
                        name,
                        errors,
                        warnings,
                        others,
                    } => {
                        let open = !self.closed.contains(path);
                        let count = |many: usize, color: gpui::Hsla| {
                            (many > 0).then(|| {
                                div().flex_none().text_color(color).child(many.to_string())
                            })
                        };
                        row.pl_2()
                            .text_color(theme.fg)
                            .child(crate::icons::draw(if open {
                                "arrow-down"
                            } else {
                                "arrow-right"
                            }))
                            .child(div().min_w_0().truncate().child(name.clone()))
                            .children(count(*errors, theme.error))
                            .children(count(*warnings, theme.warning))
                            .children(count(*others, theme.fg_subtle))
                    }
                    Row::Problem(_, problem) => {
                        let color = match problem.severity {
                            Severity::Error => theme.error,
                            Severity::Warning => theme.warning,
                            _ => theme.fg_subtle,
                        };
                        let place = format!(
                            "{}:{}",
                            problem.range.start.line + 1,
                            problem.range.start.character + 1
                        );
                        // A message of several lines is its first here.
                        let said = problem
                            .message
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_string();
                        row.pl(px(28.))
                            .text_color(theme.fg_muted)
                            .child(div().flex_none().size(px(6.)).rounded(px(3.)).bg(color))
                            .child(div().min_w_0().truncate().child(said))
                            .children(problem.source.clone().map(|source| {
                                div()
                                    .flex_none()
                                    .text_size(UI_FONT_SMALL)
                                    .text_color(theme.fg_subtle)
                                    .child(source)
                            }))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(UI_FONT_SMALL)
                                    .text_color(theme.fg_subtle)
                                    .child(place),
                            )
                    }
                }
                .into_any_element()
            })
            .collect()
    }
}

impl Render for ProblemsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (errors, warnings, files) = self.totals;
        let said = match files {
            0 => "No problems reported".to_string(),
            1 => format!("{errors} errors, {warnings} warnings in 1 file"),
            files => format!("{errors} errors, {warnings} warnings in {files} files"),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context("Problems")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::collapse))
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .text_size(UI_FONT_SIZE)
                    .text_color(theme.fg_muted)
                    .child(said),
            )
            .child(
                uniform_list(
                    "problems",
                    self.rows.len(),
                    cx.processor(|this, range: Range<usize>, window, cx| {
                        this.list(range, window, cx)
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1(),
            )
    }
}
