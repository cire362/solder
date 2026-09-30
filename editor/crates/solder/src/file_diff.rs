use std::{ops::Range, path::Path};

use gpui::{
    App, Bounds, Context, ElementInputHandler, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, HighlightStyle, KeyBinding, Pixels, Point, Render, ScrollStrategy, ScrollWheelEvent,
    SharedString, StyledText, UTF16Selection, UniformListScrollHandle, Window, actions, div,
    prelude::*, px,
};
use syntax::{HighlightKind, SyntaxTree};
use text::{Rope, diff::diff_lines};

use crate::{
    git::{DiffScope, FileDiffSnapshot, GitError},
    settings::Settings,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(
    file_diff,
    [
        NextChange,
        PreviousChange,
        Refresh,
        Close,
        OpenFile,
        ScrollLeft,
        ScrollRight
    ]
);

pub fn bind_keys(cx: &mut App) {
    let context = Some("FileDiff");
    cx.bind_keys([
        KeyBinding::new("alt-down", NextChange, context),
        KeyBinding::new("alt-up", PreviousChange, context),
        KeyBinding::new("secondary-r", Refresh, context),
        KeyBinding::new("escape", Close, context),
        KeyBinding::new("enter", OpenFile, context),
        KeyBinding::new("alt-left", ScrollLeft, context),
        KeyBinding::new("alt-right", ScrollRight, context),
    ]);
}

pub enum FileDiffEvent {
    Close,
    Refresh,
    OpenFile,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub changed: bool,
}

struct DiffLine {
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightKind)>,
}

pub struct DiffModel {
    old: Vec<DiffLine>,
    new: Vec<DiffLine>,
    pub rows: Vec<DiffRow>,
    pub changes: Vec<usize>,
    added: usize,
    removed: usize,
    width_columns: usize,
    old_no_newline: bool,
    new_no_newline: bool,
    can_open: bool,
}

impl DiffModel {
    pub fn new(snapshot: FileDiffSnapshot, tab_width: usize) -> Self {
        let old: Vec<_> = snapshot.old.split_inclusive('\n').collect();
        let new: Vec<_> = snapshot.new.split_inclusive('\n').collect();
        let mut rows = Vec::new();
        let mut changes = Vec::new();
        let mut added = 0;
        let mut removed = 0;
        let mut old_at = 0;
        let mut new_at = 0;
        for hunk in diff_lines(&old, &new) {
            while old_at < hunk.old.start {
                rows.push(DiffRow {
                    old: Some(old_at),
                    new: Some(new_at),
                    changed: false,
                });
                old_at += 1;
                new_at += 1;
            }
            changes.push(rows.len());
            added += hunk.new.len();
            removed += hunk.old.len();
            for offset in 0..hunk.old.len().max(hunk.new.len()) {
                rows.push(DiffRow {
                    old: (offset < hunk.old.len()).then_some(hunk.old.start + offset),
                    new: (offset < hunk.new.len()).then_some(hunk.new.start + offset),
                    changed: true,
                });
            }
            old_at = hunk.old.end;
            new_at = hunk.new.end;
        }
        while old_at < old.len() {
            rows.push(DiffRow {
                old: Some(old_at),
                new: Some(new_at),
                changed: false,
            });
            old_at += 1;
            new_at += 1;
        }
        let old = highlighted_lines(&snapshot.old, Path::new(&snapshot.path), tab_width);
        let new = highlighted_lines(&snapshot.new, Path::new(&snapshot.path), tab_width);
        let width_columns = old
            .iter()
            .chain(&new)
            .map(|line| line.text.chars().count())
            .max()
            .unwrap_or(0);
        Self {
            old,
            new,
            rows,
            changes,
            added,
            removed,
            width_columns,
            old_no_newline: !snapshot.old.is_empty() && !snapshot.old.ends_with('\n'),
            new_no_newline: !snapshot.new.is_empty() && !snapshot.new.ends_with('\n'),
            can_open: snapshot.can_open,
        }
    }
}

fn highlighted_lines(source: &str, path: &Path, tab_width: usize) -> Vec<DiffLine> {
    if source.is_empty() {
        return Vec::new();
    }
    let rope = Rope::from_str(source);
    let tree =
        syntax::language_for_path(path).and_then(|language| SyntaxTree::parse(language, &rope));
    let mut result: Vec<_> = source
        .split_inclusive('\n')
        .map(|line| DiffLine {
            text: line
                .trim_end_matches(['\r', '\n'])
                .replace('\t', &" ".repeat(tab_width))
                .into(),
            highlights: Vec::new(),
        })
        .collect();
    if let Some(tree) = tree {
        for (range, kind) in tree.highlights(&rope, 0..rope.len_bytes()) {
            let first_row = rope.byte_to_line(range.start);
            let last_row = rope.byte_to_line(range.end.saturating_sub(1));
            for (offset, line) in result[first_row..=last_row].iter_mut().enumerate() {
                let row = first_row + offset;
                let start = rope.line_to_byte(row);
                let raw = rope.line(row).to_string();
                let raw = raw.trim_end_matches(['\r', '\n']);
                let from = range.start.saturating_sub(start).min(raw.len());
                let to = range.end.saturating_sub(start).min(raw.len());
                let expanded = |offset| {
                    offset
                        + raw[..offset].bytes().filter(|byte| *byte == b'\t').count()
                            * tab_width.saturating_sub(1)
                };
                if from < to {
                    line.highlights.push((expanded(from)..expanded(to), kind));
                }
            }
        }
    }
    result
}

pub struct FileDiff {
    pub path: std::path::PathBuf,
    pub scope: DiffScope,
    pub model: Option<DiffModel>,
    error: Option<String>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    selected: Option<usize>,
    horizontal: Pixels,
    column_width: Pixels,
}

impl EventEmitter<FileDiffEvent> for FileDiff {}

impl FileDiff {
    #[cfg(test)]
    pub fn selected_change(&self) -> Option<usize> {
        self.selected
    }

    #[cfg(test)]
    pub fn horizontal_offset(&self) -> Pixels {
        self.horizontal
    }

    pub fn new(path: std::path::PathBuf, scope: DiffScope, cx: &mut Context<Self>) -> Self {
        Self {
            path,
            scope,
            model: None,
            error: None,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            selected: None,
            horizontal: px(0.),
            column_width: px(0.),
        }
    }

    pub fn set_result(&mut self, result: Result<DiffModel, GitError>, cx: &mut Context<Self>) {
        match result {
            Ok(model) => {
                self.model = Some(model);
                self.error = None;
                self.selected = None;
                self.horizontal = px(0.);
                self.step(false, cx);
            }
            Err(error) => {
                self.model = None;
                self.error = Some(error.to_string());
            }
        }
        cx.notify();
    }

    fn step(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let Some(model) = &self.model else { return };
        if model.changes.is_empty() {
            return;
        }
        let selected = match self.selected {
            None => 0,
            Some(selected) if backwards => {
                (selected + model.changes.len() - 1) % model.changes.len()
            }
            Some(selected) => (selected + 1) % model.changes.len(),
        };
        self.selected = Some(selected);
        self.scroll
            .scroll_to_item_strict(model.changes[selected], ScrollStrategy::Center);
        cx.notify();
    }

    fn next(&mut self, _: &NextChange, _: &mut Window, cx: &mut Context<Self>) {
        self.step(false, cx);
    }
    fn previous(&mut self, _: &PreviousChange, _: &mut Window, cx: &mut Context<Self>) {
        self.step(true, cx);
    }
    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(FileDiffEvent::Refresh);
    }
    fn close(&mut self, _: &Close, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(FileDiffEvent::Close);
    }
    fn open_file(&mut self, _: &OpenFile, _: &mut Window, cx: &mut Context<Self>) {
        if self.model.as_ref().is_some_and(|model| model.can_open) {
            cx.emit(FileDiffEvent::OpenFile);
        }
    }

    fn cell(&self, row: &DiffRow, before: bool, theme: &Theme, cx: &App) -> gpui::Div {
        let model = self.model.as_ref().unwrap();
        let number = if before { row.old } else { row.new };
        let line = number.map(|number| {
            if before {
                &model.old[number]
            } else {
                &model.new[number]
            }
        });
        let color = if before { theme.error } else { theme.git_added };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .when(before, |cell| cell.border_r_1().border_color(theme.line))
            .when(row.changed && line.is_some(), |cell| {
                cell.bg(color.opacity(0.1))
            })
            .when(line.is_none(), |cell| cell.bg(theme.bg_sunken))
            .child(
                div()
                    .w(px(55.))
                    .flex_none()
                    .pr_2()
                    .text_right()
                    .text_color(if row.changed { color } else { theme.fg_muted })
                    .child(number.map_or(String::new(), |number| format!("{}", number + 1))),
            )
            .child(div().w(px(14.)).flex_none().text_color(color).child(
                if row.changed && line.is_some() {
                    if before { "−" } else { "+" }
                } else {
                    ""
                },
            ))
            .children(line.map(|line| {
                div().flex_1().min_w_0().overflow_hidden().child(
                    div()
                        .flex_none()
                        .relative()
                        .left(-self.horizontal)
                        .pr_4()
                        .font_family(Settings::get(cx).buffer_font_family.clone())
                        .child(StyledText::new(line.text.clone()).with_highlights(
                            line.highlights.iter().map(|(range, kind)| {
                                (
                                    range.clone(),
                                    HighlightStyle {
                                        color: Some(theme.syntax.color(*kind)),
                                        ..Default::default()
                                    },
                                )
                            }),
                        )),
                )
            }))
    }

    fn pan(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        let maximum = self.model.as_ref().map_or(px(0.), |model| {
            px(model.width_columns as f32 * Settings::get(cx).buffer_font_size * 0.65)
        });
        self.horizontal =
            (self.horizontal + delta).clamp(px(0.), (maximum - self.column_width).max(px(0.)));
        cx.notify();
    }

    fn scroll_left(&mut self, _: &ScrollLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.pan(px(-80.), cx);
    }
    fn scroll_right(&mut self, _: &ScrollRight, _: &mut Window, cx: &mut Context<Self>) {
        self.pan(px(80.), cx);
    }
    fn horizontal_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(Settings::get(cx).line_height());
        if delta.x != px(0.) {
            self.pan(-delta.x, cx);
        }
    }
}

impl Focusable for FileDiff {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for FileDiff {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        Some(String::new())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        None
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for FileDiff {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let view = cx.entity();
        let name = self
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let summary = self.model.as_ref().map(|model| {
            if model.changes.is_empty() {
                "No changes".to_string()
            } else {
                format!(
                    "{} of {} changes   +{} / −{}",
                    self.selected.unwrap_or(0) + 1,
                    model.changes.len(),
                    model.added,
                    model.removed
                )
            }
        });
        let ready = self
            .model
            .as_ref()
            .is_some_and(|model| !model.changes.is_empty());
        let body = if let Some(model) = &self.model {
            if model.rows.is_empty() {
                div()
                    .p_4()
                    .child("Both versions are empty.")
                    .into_any_element()
            } else {
                gpui::uniform_list(
                    "diff-lines",
                    model.rows.len(),
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        let theme = cx.theme().clone();
                        range
                            .map(|index| {
                                let row = &this.model.as_ref().unwrap().rows[index];
                                div()
                                    .w_full()
                                    .h(Settings::get(cx).line_height())
                                    .flex()
                                    .child(this.cell(row, true, &theme, cx))
                                    .child(this.cell(row, false, &theme, cx))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .size_full()
                .into_any_element()
            }
        } else {
            div()
                .p_4()
                .text_color(if self.error.is_some() {
                    theme.error
                } else {
                    theme.fg_muted
                })
                .child(
                    self.error
                        .clone()
                        .unwrap_or_else(|| "Loading comparison...".into()),
                )
                .into_any_element()
        };
        let labels = match self.scope {
            DiffScope::Working => ("Index", "Working copy"),
            DiffScope::Staged => ("HEAD", "Index (staged)"),
        };
        let heading = |label: &'static str, no_newline: bool| {
            div()
                .flex_1()
                .min_w_0()
                .px_3()
                .py_2()
                .child(label)
                .when(no_newline, |heading| heading.child(" · No final newline"))
        };
        div()
            .id("file-diff")
            .key_context("FileDiff")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::scroll_left))
            .on_action(cx.listener(Self::scroll_right))
            .on_scroll_wheel(cx.listener(Self::horizontal_scroll))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.fg)
            .text_size(UI_FONT_SIZE)
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .border_b_1()
                    .border_color(theme.line)
                    .bg(theme.bg_sunken)
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .when(
                        self.model.as_ref().is_some_and(|model| model.can_open),
                        |bar| {
                            bar.child(ui::button(
                                "diff-open",
                                "Open file",
                                false,
                                &theme,
                                cx.listener(|_, _, _, cx| cx.emit(FileDiffEvent::OpenFile)),
                            ))
                        },
                    )
                    .child(ui::button(
                        "diff-refresh",
                        "Refresh",
                        false,
                        &theme,
                        cx.listener(|_, _, _, cx| cx.emit(FileDiffEvent::Refresh)),
                    ))
                    .child(ui::button(
                        "diff-close",
                        "Close",
                        false,
                        &theme,
                        cx.listener(|_, _, _, cx| cx.emit(FileDiffEvent::Close)),
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(summary.unwrap_or_default()))
                    .when(ready, |bar| {
                        bar.child(ui::button(
                            "diff-prev",
                            "Previous",
                            false,
                            &theme,
                            cx.listener(|this, _, _, cx| this.step(true, cx)),
                        ))
                        .child(ui::button(
                            "diff-next",
                            "Next",
                            false,
                            &theme,
                            cx.listener(|this, _, _, cx| this.step(false, cx)),
                        ))
                    }),
            )
            .child(
                div().id("diff-body").flex_1().min_h_0().min_w_0().child(
                    div()
                        .w_full()
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .bg(theme.bg_elev)
                                .border_b_1()
                                .border_color(theme.line)
                                .child(heading(
                                    labels.0,
                                    self.model
                                        .as_ref()
                                        .is_some_and(|model| model.old_no_newline),
                                ))
                                .child(heading(
                                    labels.1,
                                    self.model
                                        .as_ref()
                                        .is_some_and(|model| model.new_no_newline),
                                )),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .font_family(Settings::get(cx).buffer_font_family.clone())
                                .text_size(Settings::get(cx).buffer_font_size())
                                .line_height(Settings::get(cx).line_height())
                                .whitespace_nowrap()
                                .child(body),
                        ),
                ),
            )
            .child(
                gpui::canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        let focus = view.read(cx).focus.clone();
                        view.update(cx, |view, _| {
                            view.column_width = (bounds.size.width / 2. - px(69.)).max(px(0.))
                        });
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, view.clone()),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(old: &str, new: &str) -> DiffModel {
        DiffModel::new(
            FileDiffSnapshot {
                path: "test.rs".into(),
                old: old.into(),
                new: new.into(),
                can_open: true,
            },
            4,
        )
    }

    #[test]
    fn aligns_insertions_deletions_and_replacements() {
        let diff = model("a\nb\nc\nd\ne", "a\ninsert\nb\nchanged\ne");
        assert_eq!(
            diff.rows,
            vec![
                DiffRow {
                    old: Some(0),
                    new: Some(0),
                    changed: false
                },
                DiffRow {
                    old: None,
                    new: Some(1),
                    changed: true
                },
                DiffRow {
                    old: Some(1),
                    new: Some(2),
                    changed: false
                },
                DiffRow {
                    old: Some(2),
                    new: Some(3),
                    changed: true
                },
                DiffRow {
                    old: Some(3),
                    new: None,
                    changed: true
                },
                DiffRow {
                    old: Some(4),
                    new: Some(4),
                    changed: false
                },
            ]
        );
        assert_eq!(diff.changes, vec![1, 3]);
        assert_eq!((diff.added, diff.removed), (2, 2));
    }

    #[test]
    fn empty_files_and_missing_final_newlines() {
        assert!(model("", "").rows.is_empty());
        let added = model("", "new\n");
        assert_eq!((added.added, added.removed), (1, 0));
        assert_eq!(added.rows.len(), 1);
        assert!(added.rows.iter().all(|row| row.old.is_none()));
        let removed = model("old\n", "");
        assert_eq!((removed.added, removed.removed), (0, 1));
        assert_eq!(removed.rows.len(), 1);
        assert!(removed.rows.iter().all(|row| row.new.is_none()));
        let diff = model("a\r\n", "a");
        assert!(!diff.old_no_newline);
        assert!(diff.new_no_newline);
        assert_eq!(diff.old[0].text.as_ref(), "a");
    }

    #[test]
    fn final_newline_changes_modify_the_last_real_row() {
        for (old, new) in [("a\n", "a"), ("a", "a\n")] {
            let diff = model(old, new);
            assert_eq!((diff.added, diff.removed), (1, 1));
            assert_eq!(diff.changes, vec![0]);
            assert_eq!(diff.rows.len(), 1);
            assert_eq!((diff.rows[0].old, diff.rows[0].new), (Some(0), Some(0)));
            assert_eq!(diff.old[0].text.as_ref(), diff.new[0].text.as_ref());
            assert_ne!(diff.old_no_newline, diff.new_no_newline);
        }
        let diff = model("a\n\n", "a\n");
        assert_eq!((diff.added, diff.removed), (0, 1));
        assert_eq!(diff.rows.len(), 2);
    }

    #[test]
    fn unicode_and_tab_highlights_have_valid_boundaries() {
        let diff = model("", "\tlet café = \"🌱\";\n");
        assert!(diff.new[0].text.starts_with("    let"));
        assert!(!diff.new[0].highlights.is_empty());
        for (range, _) in &diff.new[0].highlights {
            assert!(diff.new[0].text.is_char_boundary(range.start));
            assert!(diff.new[0].text.is_char_boundary(range.end));
            assert!(range.end <= diff.new[0].text.len());
        }
    }
}
