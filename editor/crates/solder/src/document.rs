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
use text::{Buffer, Selection};

/// Files up to this size parse on the UI thread when opened, so the first frame
/// is already highlighted. Larger files show plain text for a moment instead.
const SYNC_PARSE_LIMIT: usize = 256 * 1024;

/// Time an incremental reparse may take on the typing path before it moves to
/// a worker thread.
const REPARSE_BUDGET: Duration = Duration::from_millis(1);

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
}

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
        };
        doc.initial_parse(cx);
        doc
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
                .and_then(syntax::language_for_path)
                .map(|l| l.name)
        })
    }

    pub fn diagnostics(&self) -> &Arc<Vec<Diagnostic>> {
        &self.diagnostics
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
            _ => "plaintext",
        }
    }

    pub fn title(&self) -> String {
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
        cx.emit(DocumentEvent::Edited { edits, origin });
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
        let Some(language) = self.path.as_deref().and_then(syntax::language_for_path) else {
            return;
        };
        if self.text.len() <= SYNC_PARSE_LIMIT {
            self.syntax = SyntaxTree::parse(language, self.text.rope());
            self.syntax_generation += 1;
            return;
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
