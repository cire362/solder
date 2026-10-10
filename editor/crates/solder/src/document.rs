//! A file's contents and everything derived from them: syntax tree, undo
//! history, dirty state. Editors are views onto a document, so the same file
//! can be open in two panes and edits show up in both.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{Context, EntityId, EventEmitter, Task};
use syntax::SyntaxTree;
use text::{
    Buffer, Selection,
    diff::{Hunk, diff_lines, lines},
};

/// Files up to this size parse on the UI thread when opened, so the first frame
/// is already highlighted. Larger files show plain text for a moment instead.
const SYNC_PARSE_LIMIT: usize = 256 * 1024;

/// Time an incremental reparse may take on the typing path before it moves to
/// a worker thread.
const REPARSE_BUDGET: Duration = Duration::from_millis(1);
/// How long the first parse of a small file may hold the thread that
/// draws. A file that small parses in a fraction of it.
const SYNC_BUDGET: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// Byte range in the current text.
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    /// The language server that reported it. A file may have several, and
    /// each replaces only its own.
    pub server: &'static str,
}

/// Text a language server puts into a line that is not in the file: the
/// type of a variable, the name of a parameter. It is drawn before the
/// character at `offset`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inlay {
    pub offset: usize,
    pub text: String,
}

pub enum DocumentEvent {
    /// Edits in the order they were applied. `origin` is the editor that made
    /// them, so other views know to move their cursors.
    Edited {
        edits: Arc<[text::Edit]>,
        origin: Option<EntityId>,
    },
    DirtyChanged,
    PathChanged,
    Saved,
    DiagnosticsChanged,
    /// What a language server draws into the text changed: its hints, or
    /// what it says each word is.
    HintsChanged,
    /// Git hunks or conflict regions were recomputed.
    GitChanged,
}

/// A merge conflict in the text, by row: `<<<<<<<`, the optional `|||||||`
/// base marker, `=======` and `>>>>>>>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub start: usize,
    pub base: Option<usize>,
    pub middle: usize,
    pub end: usize,
}

impl Conflict {
    pub fn ours(&self) -> Range<usize> {
        self.start + 1..self.base.unwrap_or(self.middle)
    }

    pub fn theirs(&self) -> Range<usize> {
        self.middle + 1..self.end
    }
}

/// Finds conflict blocks. Unfinished or nested markers are ignored.
pub fn parse_conflicts(text: &str) -> Vec<Conflict> {
    let mut out = Vec::new();
    let mut open: Option<(usize, Option<usize>, Option<usize>)> = None;
    for (row, line) in text.split('\n').enumerate() {
        if line.starts_with("<<<<<<<") {
            open = Some((row, None, None));
        } else if line.starts_with("|||||||") {
            if let Some((_, base @ None, None)) = open.as_mut() {
                *base = Some(row);
            }
        } else if line.starts_with("=======") {
            if let Some((_, _, middle @ None)) = open.as_mut() {
                *middle = Some(row);
            }
        } else if line.starts_with(">>>>>>>")
            && let Some((start, base, Some(middle))) = open.take()
        {
            out.push(Conflict {
                start,
                base,
                middle,
                end: row,
            });
        }
    }
    out
}

/// How long git state waits after the last edit before recomputing.
const GIT_DEBOUNCE: Duration = Duration::from_millis(120);

impl EventEmitter<DocumentEvent> for Document {}

pub struct Document {
    text: Buffer,
    path: Option<PathBuf>,
    syntax: Option<SyntaxTree>,
    /// Bumped whenever `syntax` changes, so cached highlights know they are old.
    syntax_generation: u64,
    parse_task: Option<Task<()>>,
    indent_unit: &'static str,
    was_dirty: bool,
    /// Sorted by start.
    diagnostics: Arc<Vec<Diagnostic>>,
    /// Sorted by where they are.
    server_inlays: Arc<Vec<Inlay>>,
    inlays: Arc<Vec<Inlay>>,
    decorations:
        std::collections::BTreeMap<(String, String), Vec<crate::extension_decorations::Decoration>>,
    decoration_ranges: Vec<crate::extension_decorations::Decoration>,
    /// What a language server says each word is, sorted, none over
    /// another. Counted so that what was drawn from it can be told stale.
    semantic: Arc<Vec<(Range<usize>, syntax::HighlightKind)>>,
    semantic_generation: u64,
    /// The staged version of the file; hunks are relative to it.
    diff_base: Option<Arc<str>>,
    hunks: Arc<Vec<Hunk>>,
    conflicts: Arc<Vec<Conflict>>,
    git_task: Option<Task<()>>,
    read_only: bool,
    /// Title for documents without a path (a conflict side, a diff base).
    title_override: Option<String>,
    /// Picks the grammar when there is no path.
    language_path: Option<PathBuf>,
}

impl Document {
    pub fn new(path: Option<PathBuf>, content: &str, cx: &mut Context<Self>) -> Self {
        let text = Buffer::new(content);
        let fallback = cx
            .try_global::<crate::settings::Settings>()
            .map_or("    ", |s| s.indent_unit());
        let indent_unit = detect_indent(&text, fallback);
        let mut doc = Self {
            text,
            path,
            syntax: None,
            syntax_generation: 0,
            parse_task: None,
            indent_unit,
            was_dirty: false,
            diagnostics: Arc::default(),
            server_inlays: Arc::default(),
            inlays: Arc::default(),
            decorations: Default::default(),
            decoration_ranges: Vec::new(),
            semantic: Arc::default(),
            semantic_generation: 0,
            diff_base: None,
            hunks: Arc::default(),
            conflicts: Arc::default(),
            git_task: None,
            read_only: false,
            title_override: None,
            language_path: None,
        };
        doc.initial_parse(cx);
        doc.conflicts = Arc::new(parse_conflicts(content));
        doc
    }

    /// A read-only document that is not a file on disk: one side of a merge
    /// conflict, for example. `language_path` picks the highlighting.
    pub fn virtual_file(
        title: String,
        language_path: PathBuf,
        content: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut doc = Self::new(None, content, cx);
        doc.read_only = true;
        doc.title_override = Some(title);
        doc.language_path = Some(language_path);
        doc.initial_parse(cx);
        doc
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn hunks(&self) -> &Arc<Vec<Hunk>> {
        &self.hunks
    }

    pub fn conflicts(&self) -> &Arc<Vec<Conflict>> {
        &self.conflicts
    }

    pub fn diff_base(&self) -> Option<&Arc<str>> {
        self.diff_base.as_ref()
    }

    /// Sets the version to diff against (the index), or `None` for files git
    /// does not track.
    pub fn set_diff_base(&mut self, base: Option<String>, cx: &mut Context<Self>) {
        let base: Option<Arc<str>> = base.map(|b| b.replace("\r\n", "\n").into());
        if base == self.diff_base {
            return;
        }
        self.diff_base = base;
        self.schedule_git_refresh(Duration::ZERO, true, cx);
    }

    /// Recomputes hunks and conflicts in the background. Documents with no
    /// base and no conflicts skip it unless `force` (a marker was just typed
    /// or pasted), so untracked files cost nothing per keystroke.
    fn schedule_git_refresh(&mut self, delay: Duration, force: bool, cx: &mut Context<Self>) {
        let idle = self.diff_base.is_none() && self.conflicts.is_empty() && self.hunks.is_empty();
        if idle && !force {
            return;
        }
        let base = self.diff_base.clone();
        let rope = self.text.rope().clone();
        let version = self.text.version();
        self.git_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let (hunks, conflicts) = cx
                .background_executor()
                .spawn(async move {
                    let current = rope.to_string();
                    let hunks = base
                        .as_deref()
                        .map(|base| diff_lines(&lines(base), &lines(&current)))
                        .unwrap_or_default();
                    (hunks, parse_conflicts(&current))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.text.version() != version {
                    return;
                }
                this.hunks = Arc::new(hunks);
                this.conflicts = Arc::new(conflicts);
                cx.emit(DocumentEvent::GitChanged);
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn text(&self) -> &Buffer {
        &self.text
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.text.is_dirty()
    }

    pub fn version(&self) -> u64 {
        self.text.version()
    }

    pub fn syntax(&self) -> Option<&SyntaxTree> {
        self.syntax.as_ref()
    }

    pub fn syntax_generation(&self) -> u64 {
        self.syntax_generation
    }

    pub fn indent_unit(&self) -> &'static str {
        self.indent_unit
    }

    pub fn indent_label(&self) -> &'static str {
        match self.indent_unit {
            "\t" => "Tabs",
            "  " => "Spaces: 2",
            _ => "Spaces: 4",
        }
    }

    pub fn language_name(&self) -> Option<&'static str> {
        self.syntax.as_ref().map(|s| s.language().name).or_else(|| {
            self.path
                .as_deref()
                .or(self.language_path.as_deref())
                .and_then(syntax::language_for_path)
                .map(|l| l.name)
        })
    }

    /// What starts a line comment at `offset`. Inside a file that mixes
    /// languages this is the one under the cursor: `//` in the script of a
    /// component, nothing in its markup.
    pub fn line_comment_at(&self, offset: usize) -> Option<&'static str> {
        match &self.syntax {
            Some(syntax) => syntax.language_at(offset).line_comment,
            None => self
                .path
                .as_deref()
                .or(self.language_path.as_deref())
                .and_then(syntax::language_for_path)
                .and_then(|l| l.line_comment),
        }
    }

    /// The language at `offset`: the file's own, or the one a part of it is
    /// written in.
    pub fn language_at(&self, offset: usize) -> Option<Arc<syntax::Language>> {
        match &self.syntax {
            Some(syntax) => Some(syntax.language_at(offset).clone()),
            None => self
                .path
                .as_deref()
                .or(self.language_path.as_deref())
                .and_then(syntax::language_for_path),
        }
    }

    /// What the language at `offset` counts as part of a word besides
    /// letters, digits and `_`.
    pub fn word_characters_at(&self, offset: usize) -> String {
        self.language_at(offset)
            .and_then(|l| l.editing().map(|e| e.word_characters.clone()))
            .unwrap_or_default()
    }

    /// The same for the word a completion goes on from.
    pub fn completion_characters_at(&self, offset: usize) -> String {
        self.language_at(offset)
            .and_then(|l| l.editing().map(|e| e.completion_characters.clone()))
            .unwrap_or_default()
    }

    /// The two ends of a comment at `offset`, for a language that has no
    /// comment that runs to the end of the line.
    pub fn block_comment_at(&self, offset: usize) -> Option<(String, String)> {
        self.language_at(offset)?.editing()?.block_comment.clone()
    }

    /// Whether `offset` is inside one of the places the language gives
    /// these names to: `string`, `comment`. Unknown while the tree is behind
    /// the text, and then the answer is no.
    pub fn in_scope(&self, offset: usize, scopes: &[String]) -> bool {
        self.syntax
            .as_ref()
            .is_some_and(|s| !s.is_stale() && s.in_scope(self.text.rope(), offset, scopes))
    }

    /// The names snippet files may use for the language at `offset`.
    pub fn language_ids_at(&self, offset: usize) -> Vec<String> {
        match &self.syntax {
            Some(syntax) => syntax.language_at(offset).ids(),
            None => self
                .path
                .as_deref()
                .or(self.language_path.as_deref())
                .and_then(syntax::language_for_path)
                .map(|l| l.ids())
                .unwrap_or_default(),
        }
    }

    /// The set of languages changed: an extension was installed or removed.
    /// A file that had no language may have one now, and the other way round.
    pub fn languages_changed(&mut self, cx: &mut Context<Self>) {
        let now = self
            .path
            .as_deref()
            .or(self.language_path.as_deref())
            .and_then(syntax::language_for_path);
        let same = match (&self.syntax, &now) {
            (Some(syntax), Some(language)) => Arc::ptr_eq(syntax.language(), language),
            // Nothing parsed yet, or still parsing: start over only if there
            // is something to parse with.
            (None, language) => language.is_none() && self.parse_task.is_none(),
            (Some(_), None) => false,
        };
        if same {
            return;
        }
        self.parse_task = None;
        self.set_syntax(None, cx);
        self.initial_parse(cx);
        // A language server may be waiting for this language.
        cx.emit(DocumentEvent::PathChanged);
    }

    pub fn diagnostics(&self) -> &Arc<Vec<Diagnostic>> {
        &self.diagnostics
    }

    pub fn inlays(&self) -> &Arc<Vec<Inlay>> {
        &self.inlays
    }

    pub fn decorations(&self) -> &[crate::extension_decorations::Decoration] {
        &self.decoration_ranges
    }

    pub fn set_decorations(
        &mut self,
        owner: &str,
        kind: &str,
        decorations: Vec<crate::extension_decorations::Decoration>,
        cx: &mut Context<Self>,
    ) {
        let key = (owner.to_string(), kind.to_string());
        if decorations.is_empty() {
            self.decorations.remove(&key);
        } else {
            self.decorations.insert(key, decorations);
        }
        self.rebuild_decorations();
        cx.emit(DocumentEvent::HintsChanged);
        cx.notify();
    }

    pub fn clear_decorations(&mut self, owner: &str, kind: Option<&str>, cx: &mut Context<Self>) {
        let before = self.decorations.len();
        self.decorations
            .retain(|(of, name), _| of != owner || kind.is_some_and(|kind| kind != name));
        if before != self.decorations.len() {
            self.rebuild_decorations();
            cx.emit(DocumentEvent::HintsChanged);
            cx.notify();
        }
    }

    fn rebuild_decorations(&mut self) {
        self.decoration_ranges = self.decorations.values().flatten().cloned().collect();
        self.decoration_ranges
            .sort_by_key(|decoration| decoration.range.start);
        let mut inlays = (*self.server_inlays).clone();
        for decoration in &self.decoration_ranges {
            for (offset, text) in [
                (decoration.range.start, &decoration.before),
                (decoration.range.end, &decoration.after),
            ] {
                if let Some(text) = text {
                    inlays.push(Inlay {
                        offset,
                        text: text.clone(),
                    });
                }
            }
        }
        inlays.sort_by_key(|inlay| inlay.offset);
        self.inlays = Arc::new(inlays);
    }

    pub fn set_inlays(&mut self, mut inlays: Vec<Inlay>, cx: &mut Context<Self>) {
        inlays.retain(|inlay| !inlay.text.is_empty());
        inlays.sort_by_key(|inlay| inlay.offset);
        if *self.server_inlays != inlays {
            self.server_inlays = Arc::new(inlays);
            self.rebuild_decorations();
            cx.emit(DocumentEvent::HintsChanged);
            cx.notify();
        }
    }

    pub fn semantic(&self) -> &Arc<Vec<(Range<usize>, syntax::HighlightKind)>> {
        &self.semantic
    }

    pub fn semantic_generation(&self) -> u64 {
        self.semantic_generation
    }

    /// What a language server says each word is. One that starts inside
    /// the one before it is left out: they are drawn side by side.
    pub fn set_semantic(
        &mut self,
        mut spans: Vec<(Range<usize>, syntax::HighlightKind)>,
        cx: &mut Context<Self>,
    ) {
        spans.retain(|(range, _)| range.start < range.end);
        spans.sort_by_key(|(range, _)| (range.start, range.end));
        let mut end = 0;
        spans.retain(|(range, _)| {
            let apart = range.start >= end;
            if apart {
                end = range.end;
            }
            apart
        });
        if *self.semantic != spans {
            self.semantic = Arc::new(spans);
            self.semantic_generation += 1;
            cx.emit(DocumentEvent::HintsChanged);
            cx.notify();
        }
    }

    pub fn set_diagnostics(&mut self, mut diagnostics: Vec<Diagnostic>, cx: &mut Context<Self>) {
        diagnostics.sort_by_key(|d| (d.range.start, d.severity));
        self.diagnostics = Arc::new(diagnostics);
        cx.emit(DocumentEvent::DiagnosticsChanged);
        cx.notify();
    }

    /// Applies edits from a language server (formatting, rename, completions'
    /// extra edits). Ranges are byte ranges in the current text; overlapping
    /// ones are dropped.
    pub fn apply_edits(
        &mut self,
        mut edits: Vec<(Range<usize>, String)>,
        origin: Option<EntityId>,
        cx: &mut Context<Self>,
    ) -> Arc<[text::Edit]> {
        edits.sort_by_key(|(r, _)| r.start);
        let mut clean: Vec<(Range<usize>, String)> = Vec::with_capacity(edits.len());
        for (range, text) in edits {
            if clean.last().is_some_and(|(r, _)| range.start < r.end) {
                continue;
            }
            clean.push((range, text));
        }
        self.text.seal_history();
        let applied = self.edit(clean, &[], origin, cx);
        self.text.seal_history();
        applied
    }

    /// Language id for `textDocument/didOpen`.
    pub fn language_id(&self) -> &'static str {
        match self.language_name() {
            Some("Rust") => "rust",
            Some("TypeScript") => "typescript",
            Some("TSX") => "typescriptreact",
            Some("JavaScript") => {
                if self
                    .path
                    .as_deref()
                    .and_then(|p| p.extension())
                    .is_some_and(|e| e == "jsx")
                {
                    "javascriptreact"
                } else {
                    "javascript"
                }
            }
            Some("JSON") => "json",
            Some("CSS") => "css",
            Some("Go") => "go",
            Some("Python") => "python",
            Some("C") => "c",
            Some("C++") => "cpp",
            Some("Markdown") => "markdown",
            Some("YAML") => "yaml",
            Some("Shell Script") => "shellscript",
            _ => "plaintext",
        }
    }

    pub fn title(&self) -> String {
        if let Some(title) = &self.title_override {
            return title.clone();
        }
        self.path
            .as_deref()
            .and_then(|p| p.file_name())
            .map_or("Untitled".into(), |n| n.to_string_lossy().into_owned())
    }

    // ------------------------------------------------------------ editing

    /// Applies non-overlapping edits as one undo step (merging with the
    /// previous step while typing quickly).
    pub fn edit(
        &mut self,
        edits: Vec<(Range<usize>, String)>,
        selections_before: &[Selection],
        origin: Option<EntityId>,
        cx: &mut Context<Self>,
    ) -> Arc<[text::Edit]> {
        if self.read_only {
            return Arc::from([]);
        }
        let applied: Arc<[text::Edit]> = self
            .text
            .edit(edits, selections_before, Instant::now())
            .into();
        self.after_change(applied.clone(), origin, cx);
        applied
    }

    pub fn set_selections_after(&mut self, selections: &[Selection]) {
        self.text.set_selections_after(selections);
    }

    pub fn seal_history(&mut self) {
        self.text.seal_history();
    }

    pub fn undo(&mut self, origin: EntityId, cx: &mut Context<Self>) -> Option<Vec<Selection>> {
        let (edits, selections) = self.text.undo()?;
        self.after_change(edits.into(), Some(origin), cx);
        Some(selections)
    }

    pub fn redo(&mut self, origin: EntityId, cx: &mut Context<Self>) -> Option<Vec<Selection>> {
        let (edits, selections) = self.text.redo()?;
        self.after_change(edits.into(), Some(origin), cx);
        Some(selections)
    }

    fn after_change(
        &mut self,
        edits: Arc<[text::Edit]>,
        origin: Option<EntityId>,
        cx: &mut Context<Self>,
    ) {
        if edits.is_empty() {
            return;
        }
        if let Some(tree) = self.syntax.as_mut() {
            for e in edits.iter() {
                tree.edit(e);
            }
        }
        self.reparse(cx);
        if !self.diagnostics.is_empty() {
            // Keep squiggles on the code they describe until the server
            // sends fresh ones.
            let moved = self
                .diagnostics
                .iter()
                .map(|d| Diagnostic {
                    range: map_offset(d.range.start, &edits)..map_offset(d.range.end, &edits),
                    ..d.clone()
                })
                .collect();
            self.diagnostics = Arc::new(moved);
        }
        // The same for what a server draws into the text: it stays with
        // the code it is about until the server answers again.
        if !self.server_inlays.is_empty() {
            let moved = self
                .server_inlays
                .iter()
                .map(|inlay| Inlay {
                    offset: map_offset(inlay.offset, &edits),
                    text: inlay.text.clone(),
                })
                .collect();
            self.server_inlays = Arc::new(moved);
        }
        for decoration in self.decorations.values_mut().flatten() {
            decoration.range = map_offset(decoration.range.start, &edits)
                ..map_offset(decoration.range.end, &edits);
        }
        if !self.server_inlays.is_empty() || !self.decorations.is_empty() {
            self.rebuild_decorations();
        }
        if !self.semantic.is_empty() {
            let moved = self
                .semantic
                .iter()
                .map(|(range, kind)| {
                    (
                        map_offset(range.start, &edits)..map_offset(range.end, &edits),
                        *kind,
                    )
                })
                .filter(|(range, _)| range.start < range.end)
                .collect();
            self.semantic = Arc::new(moved);
            self.semantic_generation += 1;
        }
        let marker_added = edits.iter().any(|e| e.new_text.contains("<<<<<<<"));
        cx.emit(DocumentEvent::Edited { edits, origin });
        self.schedule_git_refresh(GIT_DEBOUNCE, marker_added, cx);
        self.update_dirty(cx);
    }

    fn update_dirty(&mut self, cx: &mut Context<Self>) {
        if self.text.is_dirty() != self.was_dirty {
            self.was_dirty = self.text.is_dirty();
            cx.emit(DocumentEvent::DirtyChanged);
        }
    }

    // ------------------------------------------------------------ files

    /// Writes the text to its path. Resolves to whether the write succeeded.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        let Some(path) = self.path.clone() else {
            return Task::ready(false);
        };
        let text = self.text.text_for_save();
        let version = self.text.version();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::write(&path, text) })
                .await;
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    if this.text.version() == version {
                        this.text.mark_saved();
                    }
                    this.update_dirty(cx);
                    cx.emit(DocumentEvent::Saved);
                    true
                }
                Err(err) => {
                    eprintln!("save failed: {err}");
                    false
                }
            })
            .unwrap_or(false)
        })
    }

    /// Takes new contents from disk as one undoable step and marks the
    /// document clean. Callers only do this when there are no unsaved edits.
    pub fn reload(&mut self, content: &str, cx: &mut Context<Self>) {
        let normalized = content.replace("\r\n", "\n");
        if normalized == *self.text.rope() {
            return;
        }
        let len = self.text.len();
        self.text.seal_history();
        let applied: Arc<[text::Edit]> = self
            .text
            .edit([(0..len, normalized)], &[], Instant::now())
            .into();
        self.text.seal_history();
        self.text.mark_saved();
        self.after_change(applied, None, cx);
    }

    pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.path = Some(path);
        self.syntax = None;
        self.syntax_generation += 1;
        self.initial_parse(cx);
        cx.emit(DocumentEvent::PathChanged);
    }

    // ------------------------------------------------------------ syntax

    fn initial_parse(&mut self, cx: &mut Context<Self>) {
        let Some(language) = self
            .path
            .as_deref()
            .or(self.language_path.as_deref())
            .and_then(syntax::language_for_path)
        else {
            return;
        };
        // An extension's grammar is compiled on first use, which is too slow
        // for this thread whatever the size of the file.
        if self.text.len() <= SYNC_PARSE_LIMIT && language.is_ready() {
            // With a limit all the same: an extension's grammar, the
            // file's own or one inside it, may not answer at all.
            let parsed = SyntaxTree::parse_within(language.clone(), self.text.rope(), SYNC_BUDGET);
            if parsed.is_some() {
                self.syntax = parsed;
                self.syntax_generation += 1;
                return;
            }
        }
        let rope = self.text.rope().clone();
        let version = self.text.version();
        self.parse_task = Some(cx.spawn(async move |this, cx| {
            let tree = cx
                .background_executor()
                .spawn(async move { SyntaxTree::parse(language, &rope) })
                .await;
            this.update(cx, |this, cx| {
                // Edits made meanwhile would be missing from this tree; parse again.
                if this.text.version() != version {
                    this.initial_parse(cx);
                } else {
                    this.set_syntax(tree, cx);
                }
            })
            .ok();
        }));
    }

    fn set_syntax(&mut self, tree: Option<SyntaxTree>, cx: &mut Context<Self>) {
        self.syntax = tree;
        self.syntax_generation += 1;
        cx.notify();
    }

    fn reparse(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = self.syntax.as_mut() else {
            return;
        };
        self.syntax_generation += 1;
        if tree.reparse_within(self.text.rope(), REPARSE_BUDGET) {
            self.parse_task = None;
            return;
        }
        let stale = tree.clone();
        let rope = self.text.rope().clone();
        let version = self.text.version();
        self.parse_task = Some(cx.spawn(async move |this, cx| {
            let tree = cx
                .background_executor()
                .spawn(async move { stale.reparse_in_background(&rope) })
                .await;
            this.update(cx, |this, cx| {
                if this.text.version() == version && tree.is_some() {
                    this.set_syntax(tree, cx);
                }
            })
            .ok();
        }));
    }
}

/// Tabs if any line starts with one, else whichever of 2 or 4 spaces is more
/// common, else `fallback`.
fn detect_indent(buffer: &Buffer, fallback: &'static str) -> &'static str {
    let mut two = 0;
    let mut four = 0;
    for row in 0..buffer.line_count().min(1000) {
        let line = buffer.line_str(row);
        if line.starts_with('\t') {
            return "\t";
        }
        let spaces = line.bytes().take_while(|c| *c == b' ').count();
        if spaces == 2 {
            two += 1;
        } else if spaces == 4 {
            four += 1;
        }
    }
    match (two, four) {
        (0, 0) => fallback,
        (t, f) if t > f => "  ",
        _ => "    ",
    }
}

/// Moves an offset through edits made elsewhere (another view of the same
/// document). Positions inside a replaced range land at its end.
pub fn map_offset(offset: usize, edits: &[text::Edit]) -> usize {
    let mut o = offset;
    for e in edits {
        if o <= e.start {
            continue;
        }
        if o >= e.old_end {
            o = o + e.new_end - e.old_end;
        } else {
            o = e.new_end;
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_conflict_blocks() {
        let text = "a\n<<<<<<< HEAD\nours\n||||||| base\nold\n=======\ntheirs\n>>>>>>> branch\nz\n<<<<<<< x\nunfinished";
        let c = parse_conflicts(text);
        assert_eq!(
            c,
            vec![Conflict {
                start: 1,
                base: Some(3),
                middle: 5,
                end: 7
            }]
        );
        assert_eq!(c[0].ours(), 2..3);
        assert_eq!(c[0].theirs(), 6..7);
    }

    #[test]
    fn offsets_follow_edits_from_other_views() {
        let mut b = Buffer::new("hello world");
        let edits = b.edit([(0..5, "hi"), (6..11, "there!")], &[], Instant::now());
        assert_eq!(b.rope().to_string(), "hi there!");
        // A cursor after "world" stays at the end; one inside "hello" moves to
        // the end of the replacement.
        assert_eq!(map_offset(11, &edits), 9);
        assert_eq!(map_offset(3, &edits), 2);
        assert_eq!(map_offset(0, &edits), 0);
    }
}
