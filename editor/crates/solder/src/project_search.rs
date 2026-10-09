//! Search across the project (`cmd-shift-f`), using ripgrep's own crates:
//! parallel walk honoring `.gitignore`, SIMD literal search, binary skipping.

use std::{
    collections::HashSet,
    ops::Range,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle, KeyBinding,
    MouseButton, SharedString, StyledText, Subscription, Task, UniformListScrollHandle, Window,
    actions, div, prelude::*, px, uniform_list,
};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::Lossy};

use crate::{
    editor::{Editor, EditorEvent},
    project::is_excluded_dir,
    search::SearchOptions,
    settings::Settings,
    theme::{ActiveTheme, UI_FONT_SIZE},
    ui,
};

actions!(
    project_search,
    [Search, ToggleCase, ToggleWord, ToggleRegex]
);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("ProjectSearch");
    cx.bind_keys([
        KeyBinding::new("enter", Search, ctx),
        KeyBinding::new("alt-secondary-c", ToggleCase, ctx),
        KeyBinding::new("alt-secondary-w", ToggleWord, ctx),
        KeyBinding::new("alt-secondary-r", ToggleRegex, ctx),
    ]);
}

const MAX_MATCHES: usize = 10_000;
const MAX_LINE_CHARS: usize = 240;
const ROW: gpui::Pixels = px(24.);

/// Emitted when a result is clicked: open this file and select these columns.
pub struct OpenMatch {
    pub path: PathBuf,
    pub row: usize,
    pub columns: Range<usize>,
}

impl EventEmitter<OpenMatch> for ProjectSearch {}

struct LineMatch {
    row: usize,
    /// Trimmed line as displayed.
    text: SharedString,
    /// Byte offset of `text` within the original line.
    trim: usize,
    /// Match ranges in original line bytes.
    ranges: Vec<Range<usize>>,
}

struct FileMatches {
    path: Arc<str>,
    lines: Vec<LineMatch>,
}

#[derive(Clone, Copy)]
enum Row {
    File(usize),
    Line(usize, usize),
}

pub struct ProjectSearch {
    root: PathBuf,
    query: Entity<Editor>,
    options: SearchOptions,
    results: Vec<FileMatches>,
    rows: Vec<Row>,
    collapsed: HashSet<usize>,
    total: usize,
    limit_hit: bool,
    searching: bool,
    error: Option<String>,
    searched_query: String,
    task: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl ProjectSearch {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| Editor::single_line("Search files", cx));
        // Search as you type, after a short pause.
        let subscription = cx.subscribe_in(&query, window, |this, _, e, _, cx| {
            if let EditorEvent::Edited = e {
                this.schedule(Duration::from_millis(220), cx);
            }
        });
        Self {
            root,
            query,
            options: SearchOptions::default(),
            results: Vec::new(),
            rows: Vec::new(),
            collapsed: HashSet::new(),
            total: 0,
            limit_hit: false,
            searching: false,
            error: None,
            searched_query: String::new(),
            task: None,
            scroll: UniformListScrollHandle::new(),
            _subscription: subscription,
        }
    }

    /// Focuses the query, prefilled with `text` when given.
    pub fn focus_query(
        &mut self,
        text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.query.read(cx).text(cx);
        let text = text.unwrap_or(current);
        self.query.update(cx, |q, cx| q.set_text(&text, true, cx));
        window.focus(&self.query.focus_handle(cx));
    }

    #[cfg(test)]
    pub fn result_count(&self) -> (usize, usize) {
        (self.total, self.results.len())
    }

    fn schedule(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| this.run(cx)).ok();
        }));
    }

    fn search(&mut self, _: &Search, _: &mut Window, cx: &mut Context<Self>) {
        self.run(cx);
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text(cx);
        self.searched_query = query.clone();
        if query.trim().is_empty() {
            self.task = None;
            self.set_results(Vec::new(), false, cx);
            return;
        }
        let re = match self.options.build(&query) {
            Ok(re) => re,
            Err(err) => {
                self.error = Some(err);
                self.set_results(Vec::new(), false, cx);
                return;
            }
        };
        let matcher = match RegexMatcherBuilder::new()
            .case_insensitive(!self.options.case_sensitive)
            .build(&self.options.pattern(&query))
        {
            Ok(m) => m,
            Err(err) => {
                self.error = Some(err.to_string());
                self.set_results(Vec::new(), false, cx);
                return;
            }
        };
        self.error = None;
        self.searching = true;
        cx.notify();
        let root = self.root.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let (results, limit_hit) = cx
                .background_executor()
                .spawn(async move { search_tree(&root, &matcher, &re) })
                .await;
            this.update(cx, |this, cx| this.set_results(results, limit_hit, cx))
                .ok();
        }));
    }

    fn set_results(&mut self, results: Vec<FileMatches>, limit_hit: bool, cx: &mut Context<Self>) {
        self.searching = false;
        self.total = results
            .iter()
            .map(|f| f.lines.iter().map(|l| l.ranges.len()).sum::<usize>())
            .sum();
        self.results = results;
        self.limit_hit = limit_hit;
        self.collapsed.clear();
        self.rebuild_rows();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (fi, file) in self.results.iter().enumerate() {
            self.rows.push(Row::File(fi));
            if !self.collapsed.contains(&fi) {
                self.rows
                    .extend((0..file.lines.len()).map(|li| Row::Line(fi, li)));
            }
        }
    }

    fn click(&mut self, row: Row, cx: &mut Context<Self>) {
        match row {
            Row::File(fi) => {
                if !self.collapsed.remove(&fi) {
                    self.collapsed.insert(fi);
                }
                self.rebuild_rows();
                cx.notify();
            }
            Row::Line(fi, li) => {
                let file = &self.results[fi];
                let line = &file.lines[li];
                let columns = line.ranges.first().cloned().unwrap_or(0..0);
                cx.emit(OpenMatch {
                    path: self.root.join(&*file.path),
                    row: line.row,
                    columns,
                });
            }
        }
    }

    fn toggle(&mut self, f: impl FnOnce(&mut SearchOptions), cx: &mut Context<Self>) {
        f(&mut self.options);
        self.run(cx);
    }

    fn summary(&self) -> Option<String> {
        if let Some(err) = &self.error {
            return Some(err.clone());
        }
        if self.searching {
            return Some("Searching...".into());
        }
        if self.searched_query.trim().is_empty() {
            return None;
        }
        Some(match (self.total, self.results.len()) {
            (0, _) => "No results".into(),
            (m, f) => format!(
                "{m}{} {} in {f} {}",
                if self.limit_hit { "+" } else { "" },
                if m == 1 { "result" } else { "results" },
                if f == 1 { "file" } else { "files" }
            ),
        })
    }
}

fn search_tree(
    root: &std::path::Path,
    matcher: &grep_regex::RegexMatcher,
    re: &regex::Regex,
) -> (Vec<FileMatches>, bool) {
    let results = Mutex::new(Vec::new());
    let count = AtomicUsize::new(0);
    let limit_hit = AtomicBool::new(false);
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            !(e.file_type().is_some_and(|t| t.is_dir()) && is_excluded_dir(name))
        })
        .build_parallel()
        .run(|| {
            let mut searcher = SearcherBuilder::new()
                .binary_detection(BinaryDetection::quit(0))
                .line_number(true)
                .build();
            let (results, count, limit_hit) = (&results, &count, &limit_hit);
            Box::new(move |entry| {
                if limit_hit.load(Ordering::Relaxed) {
                    return ignore::WalkState::Quit;
                }
                let Ok(entry) = entry else {
                    return ignore::WalkState::Continue;
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    return ignore::WalkState::Continue;
                }
                let mut lines = Vec::new();
                let _ = searcher.search_path(
                    matcher,
                    entry.path(),
                    Lossy(|number, line| {
                        let line = line.trim_end_matches(['\n', '\r']);
                        let ranges: Vec<Range<usize>> = re
                            .find_iter(line)
                            .filter(|m| !m.is_empty())
                            .map(|m| m.range())
                            .collect();
                        if ranges.is_empty() {
                            return Ok(true);
                        }
                        let n = count.fetch_add(ranges.len(), Ordering::Relaxed) + ranges.len();
                        let trim = line.len() - line.trim_start().len();
                        let mut shown: String = line[trim..].chars().take(MAX_LINE_CHARS).collect();
                        if shown.len() < line.len() - trim {
                            shown.push('…');
                        }
                        lines.push(LineMatch {
                            row: number.saturating_sub(1) as usize,
                            text: shown.into(),
                            trim,
                            ranges,
                        });
                        if n >= MAX_MATCHES {
                            limit_hit.store(true, Ordering::Relaxed);
                            return Ok(false);
                        }
                        Ok(true)
                    }),
                );
                if !lines.is_empty()
                    && let Ok(rel) = entry.path().strip_prefix(root)
                {
                    let path: Arc<str> = rel.to_string_lossy().replace('\\', "/").into();
                    results.lock().unwrap().push(FileMatches { path, lines });
                }
                ignore::WalkState::Continue
            })
        });
    let mut results = results.into_inner().unwrap();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    (results, limit_hit.load(Ordering::Relaxed))
}

impl Focusable for ProjectSearch {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl Render for ProjectSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let options = self.options;
        let focused = self.query.focus_handle(cx).is_focused(window);
        let entity = cx.entity();
        let toggle = move |f: fn(&mut SearchOptions)| {
            let entity = entity.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| this.toggle(f, cx));
            }
        };
        div()
            .key_context("ProjectSearch")
            .on_action(cx.listener(Self::search))
            .on_action(cx.listener(|this, _: &ToggleCase, _, cx| {
                this.toggle(|o| o.case_sensitive = !o.case_sensitive, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleWord, _, cx| {
                this.toggle(|o| o.whole_word = !o.whole_word, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleRegex, _, cx| this.toggle(|o| o.regex = !o.regex, cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .px_2()
                    .pb_2()
                    .child(
                        ui::text_field(self.query.clone(), focused, &theme)
                            .flex_none()
                            .w_full(),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(ui::toggle(
                                "ps-case",
                                "Aa",
                                "Match case",
                                options.case_sensitive,
                                &theme,
                                toggle(|o| o.case_sensitive = !o.case_sensitive),
                            ))
                            .child(ui::toggle(
                                "ps-word",
                                "ab",
                                "Whole word",
                                options.whole_word,
                                &theme,
                                toggle(|o| o.whole_word = !o.whole_word),
                            ))
                            .child(ui::toggle(
                                "ps-regex",
                                ".*",
                                "Regular expression",
                                options.regex,
                                &theme,
                                toggle(|o| o.regex = !o.regex),
                            ))
                            .child(
                                div()
                                    .ml_1()
                                    .truncate()
                                    .text_size(crate::theme::text(11.5))
                                    .text_color(if self.error.is_some() {
                                        theme.error
                                    } else {
                                        theme.fg_subtle
                                    })
                                    .children(self.summary()),
                            ),
                    ),
            )
            .child(
                uniform_list(
                    "search-results",
                    self.rows.len(),
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        let theme = cx.theme().clone();
                        range
                            .map(|ix| {
                                let row = this.rows[ix];
                                let content = match row {
                                    Row::File(fi) => {
                                        let file = &this.results[fi];
                                        let (dir, name) = match file.path.rfind('/') {
                                            Some(i) => (&file.path[..i], &file.path[i + 1..]),
                                            None => ("", &file.path[..]),
                                        };
                                        let count: usize =
                                            file.lines.iter().map(|l| l.ranges.len()).sum();
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .min_w_0()
                                            .child(
                                                div().w(px(10.)).text_color(theme.fg_subtle).child(
                                                    if this.collapsed.contains(&fi) {
                                                        "▸"
                                                    } else {
                                                        "▾"
                                                    },
                                                ),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .text_color(theme.fg)
                                                    .child(name.to_string()),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .truncate()
                                                    .text_size(crate::theme::text(11.))
                                                    .text_color(theme.fg_subtle)
                                                    .child(dir.to_string()),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .px_1p5()
                                                    .rounded(px(6.))
                                                    .bg(theme.bg_elev)
                                                    .text_size(crate::theme::text(11.))
                                                    .text_color(theme.fg_muted)
                                                    .child(count.to_string()),
                                            )
                                    }
                                    Row::Line(fi, li) => {
                                        let line = &this.results[fi].lines[li];
                                        let highlights: Vec<(Range<usize>, HighlightStyle)> = line
                                            .ranges
                                            .iter()
                                            .filter_map(|r| {
                                                let s = r.start.checked_sub(line.trim)?;
                                                let e = (r.end - line.trim).min(line.text.len());
                                                (s < e
                                                    && line.text.is_char_boundary(s)
                                                    && line.text.is_char_boundary(e))
                                                .then(|| {
                                                    (
                                                        s..e,
                                                        HighlightStyle {
                                                            color: Some(theme.fg),
                                                            background_color: Some(
                                                                theme.search_active,
                                                            ),
                                                            ..Default::default()
                                                        },
                                                    )
                                                })
                                            })
                                            .collect();
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .pl(px(16.))
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .w(px(28.))
                                                    .text_size(crate::theme::text(11.))
                                                    .text_color(theme.fg_subtle)
                                                    .child((line.row + 1).to_string()),
                                            )
                                            .child(
                                                div()
                                                    .truncate()
                                                    .font_family(
                                                        Settings::get(cx)
                                                            .buffer_font_family
                                                            .clone(),
                                                    )
                                                    .text_size(crate::theme::text(12.))
                                                    .text_color(theme.fg_muted)
                                                    .child(
                                                        StyledText::new(line.text.clone())
                                                            .with_highlights(highlights),
                                                    ),
                                            )
                                    }
                                };
                                div()
                                    .id(ix)
                                    .h(crate::theme::row(ROW, cx))
                                    .mx_1p5()
                                    .px_1p5()
                                    .flex()
                                    .items_center()
                                    .rounded(px(8.))
                                    .text_size(UI_FONT_SIZE)
                                    .hover(|d| d.bg(theme.accent_soft))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| this.click(row, cx)),
                                    )
                                    .child(content)
                            })
                            .collect()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1(),
            )
    }
}
