//! Search across the project (`cmd-shift-f`), using ripgrep's own crates:
//! parallel walk honoring `.gitignore`, SIMD literal search, binary skipping.

use crate::lang::{t, tf};
use std::{
    collections::{BTreeMap, HashSet},
    io,
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
    revision: u64,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl ProjectSearch {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| Editor::localized_field("Search files", cx));
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
            revision: 0,
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
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| {
                if this.revision == revision {
                    this.run(cx);
                }
            })
            .ok();
        }));
    }

    fn search(&mut self, _: &Search, _: &mut Window, cx: &mut Context<Self>) {
        self.run(cx);
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        self.task = None;
        self.error = None;
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
        let options = self.options;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { search_project(&root, &query, options, &matcher, &re) })
                .await;
            this.update(cx, |this, cx| {
                if this.revision != revision {
                    return;
                }
                match result {
                    Ok((results, limit_hit)) => this.set_results(results, limit_hit, cx),
                    Err(error) => {
                        this.error = Some(tf("Could not search files: {}", &[&error]));
                        this.set_results(Vec::new(), false, cx);
                    }
                }
            })
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
            return Some(t("Searching...").into());
        }
        if self.searched_query.trim().is_empty() {
            return None;
        }
        Some(match (self.total, self.results.len()) {
            (0, _) => t("No results").into(),
            (m, f) => format!(
                "{m}{} {} in {f} {}",
                if self.limit_hit { "+" } else { "" },
                if m == 1 { "result" } else { "results" },
                if f == 1 { "file" } else { "files" }
            ),
        })
    }
}

/// All dispatch and RPC happens on the background executor. Shared container
/// files keep the native walker; an SSH cache is never a content source.
fn search_project(
    root: &std::path::Path,
    query: &str,
    options: SearchOptions,
    matcher: &grep_regex::RegexMatcher,
    re: &regex::Regex,
) -> io::Result<(Vec<FileMatches>, bool)> {
    match crate::remote::mount(root)? {
        Some(mount) if mount.is_mirror() => remote_results(mount.search(
            root,
            query,
            options.case_sensitive,
            options.whole_word,
            options.regex,
        )?),
        _ => Ok(search_tree(root, matcher, re)),
    }
}

#[derive(serde::Deserialize)]
struct RemoteMatches {
    matches: Vec<RemoteMatch>,
    limited: bool,
}

#[derive(serde::Deserialize)]
struct RemoteMatch {
    path: String,
    row: usize,
    text: String,
    ranges: Vec<[usize; 2]>,
}

fn remote_results(reply: serde_json::Value) -> io::Result<(Vec<FileMatches>, bool)> {
    let reply: RemoteMatches = serde_json::from_value(reply).map_err(io::Error::other)?;
    let mut files: BTreeMap<String, Vec<LineMatch>> = BTreeMap::new();
    let mut count = 0;
    let mut limited = reply.limited;
    for matched in reply.matches {
        // These paths are relative to the searched scope, not target absolutes.
        // Invalid paths/ranges cannot become an OpenMatch or a highlight.
        if matched.path.is_empty()
            || matched
                .path
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
            || std::path::Path::new(&matched.path)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || matched.path.contains('\0')
            || matched.row == usize::MAX
            || matched.ranges.iter().any(|[start, end]| {
                start >= end
                    || *end > matched.text.len()
                    || !matched.text.is_char_boundary(*start)
                    || !matched.text.is_char_boundary(*end)
            })
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if matched.path.split('/').any(is_excluded_dir) || matched.ranges.is_empty() {
            continue;
        }
        let available = MAX_MATCHES - count;
        limited |= matched.ranges.len() > available;
        let ranges: Vec<_> = matched
            .ranges
            .into_iter()
            .take(available)
            .map(|[start, end]| start..end)
            .collect();
        count += ranges.len();
        let trim = matched.text.len() - matched.text.trim_start().len();
        let mut shown: String = matched.text[trim..].chars().take(MAX_LINE_CHARS).collect();
        if shown.len() < matched.text.len() - trim {
            shown.push('…');
        }
        files.entry(matched.path).or_default().push(LineMatch {
            row: matched.row,
            text: shown.into(),
            trim,
            ranges,
        });
        if count >= MAX_MATCHES {
            limited = true;
            break;
        }
    }
    let results = files
        .into_iter()
        .map(|(path, mut lines)| {
            lines.sort_by_key(|line| line.row);
            FileMatches {
                path: path.into(),
                lines,
            }
        })
        .collect();
    Ok((results, limited))
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
                                t("Match case"),
                                options.case_sensitive,
                                &theme,
                                toggle(|o| o.case_sensitive = !o.case_sensitive),
                            ))
                            .child(ui::toggle(
                                "ps-word",
                                "ab",
                                t("Whole word"),
                                options.whole_word,
                                &theme,
                                toggle(|o| o.whole_word = !o.whole_word),
                            ))
                            .child(ui::toggle(
                                "ps-regex",
                                ".*",
                                t("Regular expression"),
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
                                                    .rounded(theme.shape.token)
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
                                    .rounded(theme.shape.control)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn remote_search_keeps_utf8_byte_columns_and_groups_sorted_files() {
        let (files, limited) = remote_results(json!({"limited": true, "matches": [
            {"path":"z.rs", "row":9, "text":"  Жneedle", "ranges":[[4,10]]},
            {"path":"a.rs", "row":3, "text":"needle", "ranges":[[0,6]]},
            {"path":"a.rs", "row":1, "text":"needle", "ranges":[[0,6]]},
            {"path":"node_modules/pkg/a.js", "row":0, "text":"needle", "ranges":[[0,6]]}
        ]}))
        .unwrap();
        assert!(limited);
        assert_eq!(
            files.iter().map(|file| &*file.path).collect::<Vec<_>>(),
            ["a.rs", "z.rs"]
        );
        assert_eq!(
            files[0]
                .lines
                .iter()
                .map(|line| line.row)
                .collect::<Vec<_>>(),
            [1, 3]
        );
        let line = &files[1].lines[0];
        assert_eq!(line.text, "Жneedle");
        assert_eq!(line.trim, 2);
        assert_eq!(line.ranges.len(), 1);
        assert_eq!(line.ranges[0], 4..10);
    }

    #[test]
    fn remote_search_refuses_escape_paths_and_invalid_byte_ranges() {
        for path in [
            "/etc/passwd",
            "../file",
            "a/../file",
            "a//file",
            "a/./file",
            "",
        ] {
            assert!(
                remote_results(json!({"limited":false,"matches":[
                    {"path":path,"row":0,"text":"needle","ranges":[[0,6]]}
                ]}))
                .is_err()
            );
        }
        for ranges in [json!([[1, 2]]), json!([[0, 99]]), json!([[3, 2]])] {
            assert!(
                remote_results(json!({"limited":false,"matches":[
                    {"path":"a.rs","row":0,"text":"Жneedle","ranges":ranges}
                ]}))
                .is_err()
            );
        }
    }

    #[test]
    fn remote_search_caps_matches_and_truncates_only_display_text() {
        let text = format!("{}Ж", "n".repeat(MAX_MATCHES + 1));
        let ranges: Vec<_> = (0..MAX_MATCHES + 1)
            .map(|start| [start, start + 1])
            .collect();
        let (files, limited) = remote_results(json!({"limited":false,"matches":[
            {"path":"a.rs","row":0,"text":text,"ranges":ranges}
        ]}))
        .unwrap();
        assert!(limited);
        assert_eq!(files[0].lines[0].ranges.len(), MAX_MATCHES);
        assert_eq!(files[0].lines[0].text.chars().count(), MAX_LINE_CHARS + 1);
        assert_eq!(
            files[0].lines[0].ranges[MAX_MATCHES - 1],
            MAX_MATCHES - 1..MAX_MATCHES
        );
    }

    #[test]
    fn shared_container_search_reads_physical_files_without_a_helper() {
        let root = db::testing::dir("search-shared-container")
            .canonicalize()
            .unwrap();
        std::fs::write(root.join("a.rs"), "  Жneedle\n").unwrap();
        let mount = crate::remote::Mount::connect(
            crate::remote::Target {
                endpoint: crate::remote::Endpoint::Container {
                    program: "docker-not-run".into(),
                    id: "fixture".into(),
                    user: None,
                    env: Vec::new(),
                },
                path: "/workspace/project".into(),
            },
            root.clone(),
            false,
        )
        .unwrap();
        let options = SearchOptions::default();
        let matcher = RegexMatcherBuilder::new().build("needle").unwrap();
        let re = options.build("needle").unwrap();
        let (files, _) = search_project(&root, "needle", options, &matcher, &re).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].lines[0].ranges.len(), 1);
        assert_eq!(files[0].lines[0].ranges[0], 4..10);
        drop(mount);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn closed_ssh_search_cannot_read_cached_matches() {
        let root = db::testing::dir("search-closed-ssh")
            .canonicalize()
            .unwrap();
        let source = root.join("server");
        let cache = root.join("cache");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("a.rs"), "target").unwrap();
        let mount = crate::remote::fixture(&source, &cache);
        std::fs::write(cache.join("a.rs"), "needle in stale cache").unwrap();
        drop(mount);
        let options = SearchOptions::default();
        let matcher = RegexMatcherBuilder::new().build("needle").unwrap();
        let re = options.build("needle").unwrap();
        let result = search_project(&cache, "needle", options, &matcher, &re);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::NotConnected));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ssh_search_uses_target_scope_bytes_and_query_options() {
        use std::os::unix::fs::PermissionsExt;
        let root = db::testing::dir("search-target-rpc")
            .canonicalize()
            .unwrap();
        let source = root.join("server");
        let cache = root.join("cache");
        let bin = root.join("bin");
        std::fs::create_dir_all(source.join("src")).unwrap();
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(source.join("src/file.txt"), "  Ж NEEDLE\n").unwrap();
        // CI need not have ripgrep installed. This executable checks the argv
        // the real worker gives it and returns that worker's JSON input format.
        let rg = bin.join("rg");
        std::fs::write(
            &rg,
            r#"#!/usr/bin/env python3
import json, sys
assert '--fixed-strings' in sys.argv
assert '--ignore-case' in sys.argv
assert '--word-regexp' in sys.argv
query = sys.argv[sys.argv.index('-e') + 1]
assert query == 'needle'
with open('file.txt', encoding='utf-8') as file:
    text = file.read()
start = len(text[:text.lower().index(query)].encode('utf-8'))
print(json.dumps({'type': 'match', 'data': {
    'path': {'text': './file.txt'}, 'lines': {'text': text}, 'line_number': 1,
    'submatches': [{'start': start, 'end': start + len(query)}]}}))
"#,
        )
        .unwrap();
        std::fs::set_permissions(&rg, std::fs::Permissions::from_mode(0o755)).unwrap();
        let launcher = root.join("python-fixture");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nPATH={}:\"$PATH\"\nexport PATH\nexec python3 \"$@\"\n",
                crate::remote::quoted(bin.to_str().unwrap())
            ),
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mount = crate::remote::Mount::connect(
            crate::remote::Target {
                endpoint: crate::remote::Endpoint::Process { program: launcher },
                path: source.to_str().unwrap().into(),
            },
            cache.clone(),
            true,
        )
        .unwrap();
        std::fs::write(cache.join("src/file.txt"), "needle from stale cache").unwrap();
        let options = SearchOptions {
            whole_word: true,
            ..SearchOptions::default()
        };
        let re = options.build("needle").unwrap();
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(true)
            .build(&options.pattern("needle"))
            .unwrap();
        let (files, limited) =
            search_project(&cache.join("src"), "needle", options, &matcher, &re).unwrap();
        assert!(!limited);
        assert_eq!(&*files[0].path, "file.txt");
        assert_eq!(files[0].lines[0].text, "Ж NEEDLE");
        assert_eq!(files[0].lines[0].row, 0);
        assert_eq!(files[0].lines[0].ranges.len(), 1);
        assert_eq!(files[0].lines[0].ranges[0], 5..11);
        assert_eq!(
            std::fs::read_to_string(cache.join("src/file.txt")).unwrap(),
            "needle from stale cache"
        );
        drop(mount);
        std::fs::remove_dir_all(root).unwrap();
    }
}
