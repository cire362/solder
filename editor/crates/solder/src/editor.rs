use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    App, ClipboardItem, Context, CursorStyle, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Render, ScrollWheelEvent, SharedString, Subscription, Task, UTF16Selection,
    Window, actions, div, point, prelude::*, px,
};
use syntax::HighlightKind;
use text::{Buffer, Selection, SelectionGoal};

use crate::{
    document::{Document, DocumentEvent, map_offset},
    element::{EditorElement, LayoutSnapshot},
    perf::Perf,
    theme::ActiveTheme,
};

actions!(
    editor,
    [
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        MoveWordLeft,
        MoveWordRight,
        SelectWordLeft,
        SelectWordRight,
        MoveHome,
        MoveEnd,
        SelectHome,
        SelectEnd,
        MoveToStart,
        MoveToEnd,
        SelectToStart,
        SelectToEnd,
        PageUp,
        PageDown,
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        Newline,
        Indent,
        Outdent,
        SelectAll,
        SelectLine,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        Save,
        AddCursorAbove,
        AddCursorBelow,
        SelectNextOccurrence,
        MoveLineUp,
        MoveLineDown,
        DuplicateLine,
        ToggleComment,
        Cancel,
        ShowCompletions,
        ConfirmCompletion,
        SelectNextCompletion,
        SelectPrevCompletion,
        HideCompletions,
        GoToDefinition,
        FindReferences,
        RenameSymbol,
        FormatDocument,
        NextDiagnostic,
        PrevDiagnostic,
        ShowHover,
        CodeActions,
        StageLines,
        RevertHunk,
        NextHunk,
        PrevHunk,
        AcceptOurs,
        AcceptTheirs,
        AcceptBoth,
        NextConflict,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("Editor");
    // Keys that mean something else in a text field (pickers and the find bar
    // use them for navigation and confirm) only bind in the full editor.
    let full = Some("Editor && mode == full");
    cx.bind_keys([
        KeyBinding::new("left", MoveLeft, ctx),
        KeyBinding::new("right", MoveRight, ctx),
        KeyBinding::new("up", MoveUp, full),
        KeyBinding::new("down", MoveDown, full),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-up", SelectUp, full),
        KeyBinding::new("shift-down", SelectDown, full),
        KeyBinding::new("alt-left", MoveWordLeft, ctx),
        KeyBinding::new("alt-right", MoveWordRight, ctx),
        KeyBinding::new("alt-shift-left", SelectWordLeft, ctx),
        KeyBinding::new("alt-shift-right", SelectWordRight, ctx),
        KeyBinding::new("home", MoveHome, ctx),
        KeyBinding::new("end", MoveEnd, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        KeyBinding::new("pageup", PageUp, full),
        KeyBinding::new("pagedown", PageDown, full),
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("shift-backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("alt-backspace", DeleteWordLeft, ctx),
        KeyBinding::new("alt-delete", DeleteWordRight, ctx),
        KeyBinding::new("enter", Newline, full),
        KeyBinding::new("shift-enter", Newline, full),
        KeyBinding::new("tab", Indent, full),
        KeyBinding::new("shift-tab", Outdent, full),
        KeyBinding::new("escape", Cancel, full),
        KeyBinding::new("alt-up", MoveLineUp, full),
        KeyBinding::new("alt-down", MoveLineDown, full),
        KeyBinding::new("alt-shift-down", DuplicateLine, full),
        KeyBinding::new("secondary-a", SelectAll, ctx),
        KeyBinding::new("secondary-l", SelectLine, full),
        KeyBinding::new("secondary-c", Copy, ctx),
        KeyBinding::new("secondary-x", Cut, ctx),
        KeyBinding::new("secondary-v", Paste, ctx),
        KeyBinding::new("secondary-z", Undo, ctx),
        KeyBinding::new("secondary-shift-z", Redo, ctx),
        KeyBinding::new("secondary-s", Save, full),
        KeyBinding::new("secondary-d", SelectNextOccurrence, ctx),
        KeyBinding::new("secondary-/", ToggleComment, full),
        KeyBinding::new("secondary-alt-up", AddCursorAbove, full),
        KeyBinding::new("secondary-alt-down", AddCursorBelow, full),
    ]);
    let completions = Some("Editor && showing_completions");
    cx.bind_keys([
        KeyBinding::new("ctrl-space", ShowCompletions, full),
        KeyBinding::new("up", SelectPrevCompletion, completions),
        KeyBinding::new("down", SelectNextCompletion, completions),
        KeyBinding::new("enter", ConfirmCompletion, completions),
        KeyBinding::new("tab", ConfirmCompletion, completions),
        KeyBinding::new("escape", HideCompletions, completions),
        KeyBinding::new("f12", GoToDefinition, full),
        KeyBinding::new("shift-f12", FindReferences, full),
        KeyBinding::new("f2", RenameSymbol, full),
        KeyBinding::new("shift-alt-f", FormatDocument, full),
        KeyBinding::new("f8", NextDiagnostic, full),
        KeyBinding::new("shift-f8", PrevDiagnostic, full),
        KeyBinding::new("secondary-k secondary-i", ShowHover, full),
        KeyBinding::new("secondary-.", CodeActions, full),
        KeyBinding::new("secondary-alt-y", StageLines, full),
        KeyBinding::new("secondary-alt-z", RevertHunk, full),
        KeyBinding::new("secondary-alt-]", NextHunk, full),
        KeyBinding::new("secondary-alt-[", PrevHunk, full),
        KeyBinding::new("secondary-k 1", AcceptOurs, full),
        KeyBinding::new("secondary-k 2", AcceptTheirs, full),
        KeyBinding::new("secondary-k 3", AcceptBoth, full),
        KeyBinding::new("secondary-k n", NextConflict, full),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-left", MoveHome, ctx),
        KeyBinding::new("cmd-right", MoveEnd, ctx),
        KeyBinding::new("cmd-shift-left", SelectHome, ctx),
        KeyBinding::new("cmd-shift-right", SelectEnd, ctx),
        KeyBinding::new("cmd-up", MoveToStart, full),
        KeyBinding::new("cmd-down", MoveToEnd, full),
        KeyBinding::new("cmd-shift-up", SelectToStart, full),
        KeyBinding::new("cmd-shift-down", SelectToEnd, full),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, ctx),
        KeyBinding::new("ctrl-a", MoveHome, ctx),
        KeyBinding::new("ctrl-e", MoveEnd, ctx),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-left", MoveWordLeft, ctx),
        KeyBinding::new("ctrl-right", MoveWordRight, ctx),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, ctx),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, ctx),
        KeyBinding::new("ctrl-home", MoveToStart, ctx),
        KeyBinding::new("ctrl-end", MoveToEnd, ctx),
        KeyBinding::new("ctrl-shift-home", SelectToStart, ctx),
        KeyBinding::new("ctrl-shift-end", SelectToEnd, ctx),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, ctx),
        KeyBinding::new("ctrl-y", Redo, ctx),
    ]);
}

pub enum EditorEvent {
    /// The text changed.
    Edited,
    /// Dirty flag or file path changed; tab titles need a refresh.
    TitleChanged,
    /// Cursor or selection moved; the status bar shows the position.
    SelectionsChanged,
    /// Written to disk. The project watcher will report it; this lets the
    /// workspace ignore that echo.
    Saved,
    /// Go to definition or references came back from the language server.
    OpenLocations {
        title: SharedString,
        locations: Vec<crate::editor_lsp::LspLocation>,
        always_list: bool,
    },
    /// F2: the workspace asks for the new name.
    RenameRequested { current: String },
    /// Code actions for the cursor came back; the workspace shows a picker.
    ShowCodeActions {
        actions: Vec<lsp::types::CodeActionOrCommand>,
        encoding: lsp::Encoding,
    },
    /// Stage these rows of the file (the workspace owns the repository).
    StageRows { rows: Range<usize> },
    /// A rename (or other refactor) that may touch several files.
    ApplyWorkspaceEdit {
        edit: lsp::types::WorkspaceEdit,
        encoding: lsp::Encoding,
    },
}

impl EventEmitter<EditorEvent> for Editor {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditorMode {
    /// A code editor with a gutter, many lines and syntax highlighting.
    Full,
    /// A text field. Enter, Tab and vertical movement propagate to the parent,
    /// which is how pickers and the find bar get their keyboard handling.
    SingleLine,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Char,
    Word,
    Line,
}

struct Drag {
    mode: DragMode,
    /// The unit (word or line) under the initial click, kept selected while dragging.
    origin: Range<usize>,
}

/// Highlights for the rows on screen, reused until the text, tree or viewport changes.
pub(crate) struct HighlightCache {
    pub key: (u64, u64, Range<usize>),
    pub spans: Arc<Vec<(Range<usize>, HighlightKind)>>,
}

pub struct Editor {
    pub(crate) mode: EditorMode,
    pub(crate) placeholder: Option<SharedString>,
    focus_handle: FocusHandle,
    pub(crate) document: Entity<Document>,
    /// Sorted by start and never overlapping.
    pub(crate) selections: Vec<Selection>,
    /// Index of the most recently added selection. Autoscroll follows it.
    pub(crate) newest: usize,
    pub(crate) highlight_cache: Option<HighlightCache>,
    pub(crate) scroll: Point<Pixels>,
    pub(crate) autoscroll: bool,
    pub(crate) layout: Option<LayoutSnapshot>,
    pub(crate) marked_range: Option<Range<usize>>,
    drag: Option<Drag>,
    /// Ranges painted as search results, sorted. Set by the find bar.
    pub(crate) search_matches: Arc<Vec<Range<usize>>>,
    pub(crate) active_match: Option<usize>,
    pub(crate) completion: Option<crate::completion::CompletionMenu>,
    pub(crate) completion_task: Option<Task<()>>,
    pub(crate) hover: Option<crate::editor_lsp::Hover>,
    pub(crate) hover_task: Option<Task<()>>,
    pub(crate) signature: Option<crate::editor_lsp::SignatureHint>,
    pub(crate) signature_task: Option<Task<()>>,
    /// Completion from a database schema instead of a language server
    /// (query files bound to a connection).
    pub(crate) schema_source: Option<SchemaSource>,
    _document_subscription: Subscription,
}

/// The engine and current schema for completion, read when it is needed so
/// a schema that loads later is picked up.
pub type SchemaSource = std::rc::Rc<dyn Fn(&App) -> Option<(db::Engine, Arc<db::Schema>)>>;

impl Editor {
    /// An editor on a new document.
    pub fn new(path: Option<PathBuf>, content: &str, cx: &mut Context<Self>) -> Self {
        let has_path = path.is_some();
        let document = cx.new(|cx| Document::new(path, content, cx));
        if has_path {
            crate::lsp_store::LspStore::register(&document, cx);
        }
        Self::for_document(document, cx)
    }

    pub fn set_schema_source(&mut self, source: Option<SchemaSource>) {
        self.schema_source = source;
    }

    /// Another view of an existing document, e.g. the second half of a split.
    pub fn for_document(document: Entity<Document>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&document, |this, _, event, cx| match event {
            DocumentEvent::Edited { edits, origin } => {
                if *origin != Some(cx.entity_id()) {
                    this.follow_edits(edits, cx);
                }
                cx.emit(EditorEvent::Edited);
                cx.notify();
            }
            DocumentEvent::DirtyChanged | DocumentEvent::PathChanged => {
                cx.emit(EditorEvent::TitleChanged);
                cx.notify();
            }
            DocumentEvent::Saved => cx.emit(EditorEvent::Saved),
            DocumentEvent::DiagnosticsChanged | DocumentEvent::GitChanged => cx.notify(),
        });
        Self {
            mode: EditorMode::Full,
            placeholder: None,
            focus_handle: cx.focus_handle(),
            document,
            selections: vec![Selection::cursor(0)],
            newest: 0,
            highlight_cache: None,
            scroll: Point::default(),
            autoscroll: false,
            layout: None,
            marked_range: None,
            drag: None,
            search_matches: Arc::default(),
            active_match: None,
            completion: None,
            completion_task: None,
            schema_source: None,
            hover: None,
            hover_task: None,
            signature: None,
            signature_task: None,
            _document_subscription: subscription,
        }
    }

    pub fn single_line(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        let mut editor = Self::new(None, "", cx);
        editor.mode = EditorMode::SingleLine;
        editor.placeholder = Some(placeholder.into());
        editor
    }

    pub(crate) fn is_single_line(&self) -> bool {
        self.mode == EditorMode::SingleLine
    }

    pub fn document(&self) -> &Entity<Document> {
        &self.document
    }

    pub(crate) fn doc<'a>(&self, cx: &'a App) -> &'a Document {
        self.document.read(cx)
    }

    pub(crate) fn buf<'a>(&self, cx: &'a App) -> &'a Buffer {
        self.document.read(cx).text()
    }

    pub fn text(&self, cx: &App) -> String {
        self.buf(cx).rope().to_string()
    }

    /// Replaces the whole text and puts the cursor at the end, selecting it
    /// when `select` is set (so typing replaces a prefilled query).
    pub fn set_text(&mut self, text: &str, select: bool, cx: &mut Context<Self>) {
        let text = if self.is_single_line() {
            text.replace('\n', " ")
        } else {
            text.to_owned()
        };
        let len = self.buf(cx).len();
        let before = self.selections.clone();
        self.document.update(cx, |d, _| d.seal_history());
        self.apply_edits(vec![(0..len, text.clone())], &before, cx);
        let end = self.buf(cx).len();
        let selection = if select {
            Selection::new(0, end)
        } else {
            Selection::cursor(end)
        };
        self.set_selections(vec![selection], 0);
        self.selections_changed(cx);
    }

    /// Text of the newest selection, if it is non-empty and on one line.
    pub fn selected_text(&self, cx: &App) -> Option<String> {
        let r = self.newest_selection().range();
        let text = self.buf(cx).text_for_range(r);
        (!text.is_empty() && !text.contains('\n')).then_some(text)
    }

    pub fn newest_range(&self) -> Range<usize> {
        self.newest_selection().range()
    }

    pub fn rope<'a>(&self, cx: &'a App) -> &'a text::Rope {
        self.buf(cx).rope()
    }

    pub fn version(&self, cx: &App) -> u64 {
        self.buf(cx).version()
    }

    /// Selects `range` and scrolls it into view.
    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let start = self.buf(cx).clip_offset(range.start);
        let end = self.buf(cx).clip_offset(range.end);
        self.document.update(cx, |d, _| d.seal_history());
        self.set_selections(vec![Selection::new(start, end)], 0);
        self.selections_changed(cx);
    }

    /// Replaces all selections with `ranges` (sorted or not); the last is newest.
    pub fn select_ranges(&mut self, ranges: &[Range<usize>], cx: &mut Context<Self>) {
        if ranges.is_empty() {
            return;
        }
        self.document.update(cx, |d, _| d.seal_history());
        let buffer = self.buf(cx);
        let selections = ranges
            .iter()
            .map(|r| Selection::new(buffer.clip_offset(r.start), buffer.clip_offset(r.end)))
            .collect::<Vec<_>>();
        let newest = selections.len() - 1;
        self.set_selections(selections, newest);
        self.selections_changed(cx);
    }

    /// Selects byte columns `cols` of a 0-based row.
    pub fn select_in_row(&mut self, row: usize, cols: Range<usize>, cx: &mut Context<Self>) {
        let buffer = self.buf(cx);
        let row = row.min(buffer.line_count() - 1);
        let start = buffer.line_start(row);
        let len = buffer.line_len(row);
        self.select_range(start + cols.start.min(len)..start + cols.end.min(len), cx);
    }

    /// Moves the cursor to a 0-based row and column (in display cells).
    pub fn go_to_point(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        let buffer = self.buf(cx);
        let row = row.min(buffer.line_count() - 1);
        let col = buffer.column_for_display(row, column);
        let offset = buffer.line_start(row) + col;
        self.select_range(offset..offset, cx);
    }

    pub fn line_count(&self, cx: &App) -> usize {
        self.buf(cx).line_count()
    }

    pub fn set_search_matches(
        &mut self,
        matches: Arc<Vec<Range<usize>>>,
        active: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.search_matches = matches;
        self.active_match = active;
        cx.notify();
    }

    /// Applies non-overlapping replacements (used by Replace All) as one undo step.
    pub fn replace_ranges(&mut self, edits: Vec<(Range<usize>, String)>, cx: &mut Context<Self>) {
        self.document.update(cx, |d, _| d.seal_history());
        self.edit_ranges(edits, cx);
        self.document.update(cx, |d, _| d.seal_history());
    }

    pub(crate) fn focus_handle_ref(&self) -> &FocusHandle {
        &self.focus_handle
    }

    pub fn path<'a>(&self, cx: &'a App) -> Option<&'a Path> {
        self.doc(cx).path()
    }

    /// 1-based line and column of the newest cursor, plus the selection count.
    pub fn cursor_position(&self, cx: &App) -> (usize, usize, usize) {
        let head = self.selections[self.newest].head;
        let buffer = self.buf(cx);
        let p = buffer.offset_to_point(head);
        let col = buffer.display_column(p);
        (p.row + 1, col + 1, self.selections.len())
    }

    /// Another view edited the document: carry our selections through.
    fn follow_edits(&mut self, edits: &[text::Edit], cx: &mut Context<Self>) {
        let len = self.buf(cx).len();
        let mapped: Vec<Selection> = self
            .selections
            .iter()
            .map(|s| Selection {
                anchor: map_offset(s.anchor, edits).min(len),
                head: map_offset(s.head, edits).min(len),
                goal: s.goal,
            })
            .collect();
        self.set_selections(mapped, self.newest);
        if let Some(m) = self.marked_range.take() {
            self.marked_range = Some(map_offset(m.start, edits)..map_offset(m.end, edits));
        }
    }

    fn sync_selections_after(&mut self, cx: &mut Context<Self>) {
        let selections = self.selections.clone();
        self.document
            .update(cx, |d, _| d.set_selections_after(&selections));
    }

    // ---------------------------------------------------------------- selections

    fn newest_selection(&self) -> Selection {
        self.selections[self.newest]
    }

    /// Sorts, merges overlapping selections and keeps `newest` pointing at the
    /// same selection (or the one it merged into).
    fn set_selections(&mut self, mut selections: Vec<Selection>, newest: usize) {
        let marker = selections[newest];
        selections.sort_by_key(|s| s.range().start);
        let mut merged: Vec<Selection> = Vec::with_capacity(selections.len());
        let mut newest = 0;
        for s in selections {
            if let Some(last) = merged.last_mut() {
                let (lr, sr) = (last.range(), s.range());
                if sr.start < lr.end || sr.start == lr.start {
                    let range = lr.start.min(sr.start)..lr.end.max(sr.end);
                    let reversed = last.is_reversed();
                    *last = if reversed {
                        Selection::new(range.end, range.start)
                    } else {
                        Selection::new(range.start, range.end)
                    };
                    if s == marker {
                        newest = merged.len() - 1;
                    }
                    continue;
                }
            }
            if s == marker {
                newest = merged.len();
            }
            merged.push(s);
        }
        self.selections = merged;
        self.newest = newest.min(self.selections.len() - 1);
    }

    fn selections_changed(&mut self, cx: &mut Context<Self>) {
        self.autoscroll = true;
        cx.emit(EditorEvent::SelectionsChanged);
        cx.notify();
    }

    /// Moves every selection. With `extend`, anchors stay and heads move.
    fn move_selections(
        &mut self,
        extend: bool,
        cx: &mut Context<Self>,
        f: impl Fn(&Buffer, &Selection) -> (usize, SelectionGoal),
    ) {
        Perf::input_started(cx);
        self.hide_popovers(cx);
        let moved: Vec<Selection> = self
            .selections
            .iter()
            .map(|s| {
                let (head, goal) = f(self.buf(cx), s);
                Selection {
                    anchor: if extend { s.anchor } else { head },
                    head,
                    goal,
                }
            })
            .collect();
        self.set_selections(moved, self.newest);
        self.document.update(cx, |d, _| d.seal_history());
        self.selections_changed(cx);
    }

    // ---------------------------------------------------------------- editing

    /// Applies one edit per selection. Each tuple is the range to replace, the
    /// new text, and where the cursor lands inside the new text.
    fn edit_selections(
        &mut self,
        cx: &mut Context<Self>,
        f: impl Fn(&Buffer, &Selection) -> (Range<usize>, String, usize),
    ) {
        Perf::input_started(cx);
        let before = self.selections.clone();
        let mut edits: Vec<(Range<usize>, String)> = Vec::with_capacity(before.len());
        let mut cursors: Vec<usize> = Vec::with_capacity(before.len());
        let mut delta: isize = 0;
        let mut prev_end = 0;
        for s in &before {
            let (range, text, cursor) = f(self.buf(cx), s);
            if !edits.is_empty() && range.start < prev_end {
                // Overlaps the previous edit (two cursors deleting into each
                // other); the previous edit already covers it.
                cursors.push(*cursors.last().unwrap());
                continue;
            }
            let new_start = (range.start as isize + delta) as usize;
            cursors.push(new_start + cursor.min(text.len()));
            delta += text.len() as isize - range.len() as isize;
            prev_end = range.end;
            edits.push((range, text));
        }
        self.apply_edits(edits, &before, cx);
        let selections = cursors.into_iter().map(Selection::cursor).collect();
        self.set_selections(selections, self.newest);
        self.sync_selections_after(cx);
        self.selections_changed(cx);
    }

    /// Applies arbitrary edits and maps existing selections through them.
    fn edit_ranges(&mut self, mut edits: Vec<(Range<usize>, String)>, cx: &mut Context<Self>) {
        Perf::input_started(cx);
        edits.sort_by_key(|(r, _)| r.start);
        let before = self.selections.clone();
        let map = |offset: usize, bias_right: bool| {
            let mut delta: isize = 0;
            for (r, t) in &edits {
                let after = if bias_right {
                    r.start <= offset
                } else {
                    r.start < offset
                };
                if r.end <= offset && after {
                    delta += t.len() as isize - r.len() as isize;
                } else if r.start < offset {
                    return (r.start as isize + delta) as usize + t.len();
                }
            }
            (offset as isize + delta) as usize
        };
        let mapped: Vec<Selection> = before
            .iter()
            .map(|s| {
                let start_is_anchor = s.anchor <= s.head;
                let empty = s.is_empty();
                Selection {
                    anchor: map(s.anchor, empty || !start_is_anchor),
                    head: map(s.head, empty || start_is_anchor),
                    goal: SelectionGoal::None,
                }
            })
            .collect();
        self.apply_edits(edits, &before, cx);
        self.set_selections(mapped, self.newest);
        self.sync_selections_after(cx);
        self.selections_changed(cx);
    }

    fn apply_edits(
        &mut self,
        edits: Vec<(Range<usize>, String)>,
        before: &[Selection],
        cx: &mut Context<Self>,
    ) {
        let origin = cx.entity_id();
        self.document
            .update(cx, |d, cx| d.edit(edits, before, Some(origin), cx));
    }

    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = if self.is_single_line() {
            text.replace('\n', " ")
        } else {
            text.to_owned()
        };
        self.edit_selections(cx, |_, s| (s.range(), text.clone(), text.len()));
    }

    /// Typed text. Adds bracket and quote pairs, types over a closing
    /// character that is already there, and wraps selections in pairs.
    fn handle_input(&mut self, text: &str, cx: &mut Context<Self>) {
        self.insert_typed(text, cx);
        self.after_typing(text, cx);
    }

    fn insert_typed(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.is_single_line() {
            // A text field never takes a typed newline; Enter belongs to its parent.
            let text: String = text.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
            if !text.is_empty() {
                self.insert(&text, cx);
            }
            return;
        }
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return self.insert(text, cx);
        };
        let rust = self.doc(cx).language_name() == Some("Rust");
        let close = match c {
            '(' => Some(')'),
            '[' => Some(']'),
            '{' => Some('}'),
            '"' | '`' => Some(c),
            '\'' if !rust => Some(c),
            _ => None,
        };
        let closing = matches!(c, ')' | ']' | '}' | '"' | '\'' | '`');
        if close.is_none() && !closing {
            return self.insert(text, cx);
        }
        self.edit_selections(cx, |b, s| {
            let head = s.head;
            if let Some(close) = close
                && !s.is_empty()
            {
                let inner = b.text_for_range(s.range());
                let len = inner.len();
                return (s.range(), format!("{c}{inner}{close}"), 1 + len);
            }
            if s.is_empty() && closing && b.char_at(head) == Some(c) {
                return (head..head + c.len_utf8(), c.to_string(), c.len_utf8());
            }
            if let Some(close) = close
                && s.is_empty()
            {
                let next = b.char_at(head);
                let prev = b.char_before(head);
                let next_ok = next.is_none_or(|n| n.is_whitespace() || ")]},;:".contains(n));
                let quote = c == close;
                let prev_ok = !quote || prev.is_none_or(|p| !p.is_alphanumeric() && p != c);
                if next_ok && prev_ok {
                    return (head..head, format!("{c}{close}"), c.len_utf8());
                }
            }
            (s.range(), c.to_string(), c.len_utf8())
        });
    }

    // ---------------------------------------------------------------- actions

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(false, cx, |b, s| {
            let head = if s.is_empty() {
                b.prev_grapheme(s.head)
            } else {
                s.range().start
            };
            (head, SelectionGoal::None)
        });
    }

    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(false, cx, |b, s| {
            let head = if s.is_empty() {
                b.next_grapheme(s.head)
            } else {
                s.range().end
            };
            (head, SelectionGoal::None)
        });
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        self.move_selections(false, cx, |b, s| b.move_vertically(s.head, s.goal, -1));
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        self.move_selections(false, cx, |b, s| b.move_vertically(s.head, s.goal, 1));
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(true, cx, |b, s| {
            (b.prev_grapheme(s.head), SelectionGoal::None)
        });
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(true, cx, |b, s| {
            (b.next_grapheme(s.head), SelectionGoal::None)
        });
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        self.move_selections(true, cx, |b, s| b.move_vertically(s.head, s.goal, -1));
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        self.move_selections(true, cx, |b, s| b.move_vertically(s.head, s.goal, 1));
    }

    fn move_word_left(&mut self, _: &MoveWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(false, cx, |b, s| {
            (b.prev_word_start(s.head), SelectionGoal::None)
        });
    }

    fn move_word_right(&mut self, _: &MoveWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(false, cx, |b, s| {
            (b.next_word_end(s.head), SelectionGoal::None)
        });
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(true, cx, |b, s| {
            (b.prev_word_start(s.head), SelectionGoal::None)
        });
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(true, cx, |b, s| {
            (b.next_word_end(s.head), SelectionGoal::None)
        });
    }

    fn move_home(&mut self, _: &MoveHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(false, cx, |b, s| {
            (b.smart_home(s.head), SelectionGoal::None)
        });
    }

    fn move_end(&mut self, _: &MoveEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(false, cx, |b, s| (b.line_end(s.head), SelectionGoal::None));
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(true, cx, |b, s| (b.smart_home(s.head), SelectionGoal::None));
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(true, cx, |b, s| (b.line_end(s.head), SelectionGoal::None));
    }

    fn move_to_start(&mut self, _: &MoveToStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(false, cx, |_, _| (0, SelectionGoal::None));
    }

    fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(false, cx, |b, _| (b.len(), SelectionGoal::None));
    }

    fn select_to_start(&mut self, _: &SelectToStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(true, cx, |_, _| (0, SelectionGoal::None));
    }

    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selections(true, cx, |b, _| (b.len(), SelectionGoal::None));
    }

    fn visible_rows(&self) -> isize {
        self.layout
            .as_ref()
            .map_or(30, |l| {
                (f32::from(l.bounds.size.height) / f32::from(l.line_height)) as isize
            })
            .max(1)
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        let rows = self.visible_rows();
        self.move_selections(false, cx, |b, s| b.move_vertically(s.head, s.goal, -rows));
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        let rows = self.visible_rows();
        self.move_selections(false, cx, |b, s| b.move_vertically(s.head, s.goal, rows));
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        Perf::input_started(cx);
        self.set_selections(vec![Selection::new(0, self.buf(cx).len())], 0);
        self.selections_changed(cx);
    }

    /// Expands each selection to whole lines; repeating extends by one line.
    fn select_line(&mut self, _: &SelectLine, _: &mut Window, cx: &mut Context<Self>) {
        Perf::input_started(cx);
        let selections = self
            .selections
            .iter()
            .map(|s| {
                let r = s.range();
                let start = self.buf(cx).line_range_at(r.start).start;
                let end = self.buf(cx).line_range_at(r.end).end;
                Selection::new(start, end)
            })
            .collect();
        self.set_selections(selections, self.newest);
        self.selections_changed(cx);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self.hover.is_some() || self.completion.is_some() || self.signature.is_some() {
            self.hide_popovers(cx);
            return;
        }
        let s = self.newest_selection();
        if self.selections.len() == 1 && s.is_empty() {
            cx.propagate();
            return;
        }
        Perf::input_started(cx);
        let collapsed = if self.selections.len() > 1 {
            s
        } else {
            Selection::cursor(s.head)
        };
        self.set_selections(vec![collapsed], 0);
        self.selections_changed(cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let unit = self.doc(cx).indent_unit();
        self.edit_selections(cx, |b, s| {
            if !s.is_empty() {
                return (s.range(), String::new(), 0);
            }
            // Between an empty pair: delete both halves.
            if let (Some(open), Some(close)) = (b.char_before(s.head), b.char_at(s.head))
                && matches!(
                    (open, close),
                    ('(', ')') | ('[', ']') | ('{', '}') | ('"', '"') | ('\'', '\'') | ('`', '`')
                )
            {
                return (s.head - 1..s.head + 1, String::new(), 0);
            }
            // In leading whitespace, delete back to the previous indent stop.
            let p = b.offset_to_point(s.head);
            let line = b.line_str(p.row);
            let before = &line[..p.column];
            if unit != "\t" && !before.is_empty() && before.bytes().all(|c| c == b' ') {
                let width = unit.len();
                let remove = match p.column % width {
                    0 => width,
                    r => r,
                };
                return (s.head - remove..s.head, String::new(), 0);
            }
            (b.prev_grapheme(s.head)..s.head, String::new(), 0)
        });
        if self.completion.is_some() {
            self.refilter_completions(cx);
        }
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.edit_selections(cx, |b, s| {
            let range = if s.is_empty() {
                s.head..b.next_grapheme(s.head)
            } else {
                s.range()
            };
            (range, String::new(), 0)
        });
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.edit_selections(cx, |b, s| {
            let range = if s.is_empty() {
                b.prev_word_start(s.head)..s.head
            } else {
                s.range()
            };
            (range, String::new(), 0)
        });
    }

    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.edit_selections(cx, |b, s| {
            let range = if s.is_empty() {
                s.head..b.next_word_end(s.head)
            } else {
                s.range()
            };
            (range, String::new(), 0)
        });
    }

    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_selections(cx, |b, s| {
            let range = if s.is_empty() {
                b.line_range_at(s.head).start..s.head
            } else {
                s.range()
            };
            (range, String::new(), 0)
        });
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        let unit = self.doc(cx).indent_unit();
        let python = self.doc(cx).language_name() == Some("Python");
        self.edit_selections(cx, |b, s| {
            let range = s.range();
            let p = b.offset_to_point(range.start);
            let line = b.line_str(p.row);
            let indent: String = line[..p.column]
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let before = b.char_before(range.start);
            let after = b.char_at(range.end);
            let opens = matches!(before, Some('{' | '[' | '(')) || (python && before == Some(':'));
            let closes = matches!(
                (before, after),
                (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
            );
            if closes {
                let text = format!("\n{indent}{unit}\n{indent}");
                let cursor = 1 + indent.len() + unit.len();
                (range, text, cursor)
            } else if opens {
                let text = format!("\n{indent}{unit}");
                let len = text.len();
                (range, text, len)
            } else {
                let text = format!("\n{indent}");
                let len = text.len();
                (range, text, len)
            }
        });
    }

    /// Rows covered by each selection, merged and sorted. A selection ending at
    /// column 0 of a row does not include that row.
    fn selected_row_blocks(&self, cx: &App) -> Vec<Range<usize>> {
        let mut blocks: Vec<Range<usize>> = Vec::new();
        for s in &self.selections {
            let r = s.range();
            let start = self.buf(cx).offset_to_point(r.start).row;
            let end_point = self.buf(cx).offset_to_point(r.end);
            let mut end = end_point.row + 1;
            if end_point.column == 0 && end_point.row > start {
                end -= 1;
            }
            match blocks.last_mut() {
                Some(last) if start <= last.end => last.end = last.end.max(end),
                _ => blocks.push(start..end),
            }
        }
        blocks
    }

    fn indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        let unit = self.doc(cx).indent_unit();
        let multiline = self.selections.iter().any(|s| {
            self.buf(cx).offset_to_point(s.range().start).row
                != self.buf(cx).offset_to_point(s.range().end).row
        });
        if !multiline {
            self.edit_selections(cx, |b, s| {
                let text = if unit == "\t" {
                    "\t".to_string()
                } else {
                    let col = b.display_column(b.offset_to_point(s.range().start));
                    " ".repeat(unit.len() - col % unit.len())
                };
                let len = text.len();
                (s.range(), text, len)
            });
            return;
        }
        let edits = self
            .selected_row_blocks(cx)
            .into_iter()
            .flatten()
            .filter(|row| self.buf(cx).line_len(*row) > 0)
            .map(|row| {
                let at = self.buf(cx).line_start(row);
                (at..at, unit.to_string())
            })
            .collect();
        self.edit_ranges(edits, cx);
    }

    fn outdent(&mut self, _: &Outdent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        let unit = self.doc(cx).indent_unit();
        let edits = self
            .selected_row_blocks(cx)
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let line = self.buf(cx).line_str(row);
                let remove = if line.starts_with('\t') {
                    1
                } else {
                    line.bytes()
                        .take(unit.len().max(1))
                        .take_while(|c| *c == b' ')
                        .count()
                };
                let at = self.buf(cx).line_start(row);
                (remove > 0).then(|| (at..at + remove, String::new()))
            })
            .collect();
        self.edit_ranges(edits, cx);
    }

    fn toggle_comment(&mut self, _: &ToggleComment, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        let prefix = match self.doc(cx).language_name() {
            Some("Python") => "#",
            Some("Rust" | "TypeScript" | "TSX" | "JavaScript" | "Go" | "JSON") => "//",
            _ => return,
        };
        let rows: Vec<usize> = self.selected_row_blocks(cx).into_iter().flatten().collect();
        let lines: Vec<(usize, String)> = rows
            .iter()
            .map(|r| (*r, self.buf(cx).line_str(*r).into_owned()))
            .filter(|(_, l)| !l.trim().is_empty())
            .collect();
        if lines.is_empty() {
            return;
        }
        let all_commented = lines
            .iter()
            .all(|(_, l)| l.trim_start().starts_with(prefix));
        let min_indent = lines
            .iter()
            .map(|(_, l)| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        let edits = lines
            .iter()
            .map(|(row, line)| {
                let start = self.buf(cx).line_start(*row);
                if all_commented {
                    let at = line.len() - line.trim_start().len();
                    let rest = &line[at + prefix.len()..];
                    let len = prefix.len() + usize::from(rest.starts_with(' '));
                    (start + at..start + at + len, String::new())
                } else {
                    (start + min_indent..start + min_indent, format!("{prefix} "))
                }
            })
            .collect();
        self.edit_ranges(edits, cx);
    }

    fn move_lines(&mut self, up: bool, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        let last_row = self.buf(cx).line_count() - 1;
        let blocks = self.selected_row_blocks(cx);
        if blocks
            .iter()
            .any(|b| if up { b.start == 0 } else { b.end > last_row })
        {
            return;
        }
        Perf::input_started(cx);
        let before = self.selections.clone();
        let mut edits = Vec::new();
        let mut shifts: Vec<(Range<usize>, isize)> = Vec::new();
        for block in &blocks {
            let block_start = self.buf(cx).line_start(block.start);
            let block_end =
                self.buf(cx).line_start(block.end - 1) + self.buf(cx).line_len(block.end - 1);
            let block_text = self.buf(cx).text_for_range(block_start..block_end);
            if up {
                let other_start = self.buf(cx).line_start(block.start - 1);
                let other = self.buf(cx).text_for_range(other_start..block_start - 1);
                edits.push((other_start..block_end, format!("{block_text}\n{other}")));
                shifts.push((block_start..block_end, -((other.len() + 1) as isize)));
            } else {
                let other_end =
                    self.buf(cx).line_start(block.end) + self.buf(cx).line_len(block.end);
                let other = self.buf(cx).text_for_range(block_end + 1..other_end);
                edits.push((block_start..other_end, format!("{other}\n{block_text}")));
                shifts.push((block_start..block_end, (other.len() + 1) as isize));
            }
        }
        let shift = |o: usize| {
            shifts
                .iter()
                .find(|(r, _)| r.start <= o && o <= r.end)
                .map_or(o, |(_, d)| (o as isize + d) as usize)
        };
        let moved = before
            .iter()
            .map(|s| Selection {
                anchor: shift(s.anchor),
                head: shift(s.head),
                goal: s.goal,
            })
            .collect();
        self.apply_edits(edits, &before, cx);
        self.set_selections(moved, self.newest);
        self.sync_selections_after(cx);
        self.selections_changed(cx);
    }

    fn move_line_up(&mut self, _: &MoveLineUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_lines(true, cx);
    }

    fn move_line_down(&mut self, _: &MoveLineDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_lines(false, cx);
    }

    fn duplicate_line(&mut self, _: &DuplicateLine, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        let edits = self
            .selected_row_blocks(cx)
            .into_iter()
            .map(|block| {
                let start = self.buf(cx).line_start(block.start);
                let end =
                    self.buf(cx).line_start(block.end - 1) + self.buf(cx).line_len(block.end - 1);
                let text = self.buf(cx).text_for_range(start..end);
                (start..start, format!("{text}\n"))
            })
            .collect();
        self.edit_ranges(edits, cx);
    }

    fn add_cursor_vertically(&mut self, rows: isize, cx: &mut Context<Self>) {
        Perf::input_started(cx);
        let s = self.newest_selection();
        let (head, goal) = self.buf(cx).move_vertically(s.head, s.goal, rows);
        if self.buf(cx).offset_to_point(head).row == self.buf(cx).offset_to_point(s.head).row {
            return;
        }
        let mut selections = self.selections.clone();
        selections.push(Selection {
            anchor: head,
            head,
            goal,
        });
        let newest = selections.len() - 1;
        self.set_selections(selections, newest);
        self.selections_changed(cx);
    }

    fn add_cursor_above(&mut self, _: &AddCursorAbove, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        self.add_cursor_vertically(-1, cx);
    }

    fn add_cursor_below(&mut self, _: &AddCursorBelow, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            cx.propagate();
            return;
        }
        self.add_cursor_vertically(1, cx);
    }

    fn select_next_occurrence(
        &mut self,
        _: &SelectNextOccurrence,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Perf::input_started(cx);
        let newest = self.newest_selection();
        if newest.is_empty() {
            let word = self.buf(cx).word_range_at(newest.head);
            if word.is_empty() {
                return;
            }
            let mut selections = self.selections.clone();
            selections[self.newest] = Selection::new(word.start, word.end);
            self.set_selections(selections, self.newest);
            self.selections_changed(cx);
            return;
        }
        let needle = self.buf(cx).text_for_range(newest.range());
        let haystack = self.buf(cx).rope().to_string();
        let from = self.selections.last().map_or(0, |s| s.range().end);
        let found = haystack[from..]
            .find(&needle)
            .map(|i| i + from)
            .or_else(|| haystack[..from].find(&needle));
        let Some(start) = found else { return };
        let range = start..start + needle.len();
        if self.selections.iter().any(|s| s.range() == range) {
            return;
        }
        let mut selections = self.selections.clone();
        selections.push(Selection::new(range.start, range.end));
        let newest = selections.len() - 1;
        self.set_selections(selections, newest);
        self.selections_changed(cx);
    }

    fn copy_text(&self, cx: &App) -> (String, bool) {
        if self.selections.iter().all(|s| s.is_empty()) {
            // Nothing selected: copy whole lines, like most editors.
            let text: Vec<String> = self
                .selections
                .iter()
                .map(|s| {
                    let buffer = self.buf(cx);
                    buffer.slice(buffer.line_range_at(s.head)).to_string()
                })
                .collect();
            let mut joined = text.concat();
            if !joined.ends_with('\n') {
                joined.push('\n');
            }
            return (joined, true);
        }
        let text: Vec<String> = self
            .selections
            .iter()
            .map(|s| self.buf(cx).text_for_range(s.range()))
            .collect();
        (text.join("\n"), false)
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let (text, _) = self.copy_text(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        let (text, whole_lines) = self.copy_text(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if whole_lines {
            self.edit_selections(cx, |b, s| (b.line_range_at(s.head), String::new(), 0));
        } else {
            self.edit_selections(cx, |_, s| (s.range(), String::new(), 0));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let mut text = text.replace("\r\n", "\n");
        if self.is_single_line() {
            text = text.trim_end_matches('\n').replace('\n', " ");
        }
        let parts: Vec<&str> = text.split('\n').collect();
        if self.selections.len() > 1 && parts.len() == self.selections.len() {
            // One line per cursor, the inverse of a multi-cursor copy.
            let parts: Vec<String> = parts.into_iter().map(str::to_owned).collect();
            let order: Vec<Range<usize>> = self.selections.iter().map(|s| s.range()).collect();
            self.edit_selections(cx, |_, s| {
                let i = order.iter().position(|r| *r == s.range()).unwrap_or(0);
                (s.range(), parts[i].clone(), parts[i].len())
            });
        } else {
            self.insert(&text, cx);
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        Perf::input_started(cx);
        let origin = cx.entity_id();
        if let Some(selections) = self.document.update(cx, |d, cx| d.undo(origin, cx)) {
            self.restore_selections(selections, cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        Perf::input_started(cx);
        let origin = cx.entity_id();
        if let Some(selections) = self.document.update(cx, |d, cx| d.redo(origin, cx)) {
            self.restore_selections(selections, cx);
        }
    }

    fn restore_selections(&mut self, selections: Vec<Selection>, cx: &mut Context<Self>) {
        let len = self.buf(cx).len();
        let mut selections: Vec<Selection> = selections
            .into_iter()
            .map(|s| Selection::new(s.anchor.min(len), s.head.min(len)))
            .collect();
        if selections.is_empty() {
            selections.push(Selection::cursor(self.newest_selection().head.min(len)));
        }
        let newest = selections.len() - 1;
        self.set_selections(selections, newest);
        self.selections_changed(cx);
    }

    fn save_action(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        // Untitled documents bubble up so the workspace can ask for a path.
        if self.is_single_line() || self.path(cx).is_none() {
            cx.propagate();
            return;
        }
        self.save(cx).detach();
    }

    /// Writes the document to its path, formatting first when
    /// `format_on_save` is on. Resolves to whether the write succeeded.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        if !crate::settings::Settings::get(cx).format_on_save {
            return self.document.update(cx, |d, cx| d.save(cx));
        }
        let format = self.format(cx);
        cx.spawn(async move |this, cx| {
            format.await;
            let Ok(save) = this.update(cx, |e, cx| e.document.update(cx, |d, cx| d.save(cx)))
            else {
                return false;
            };
            save.await
        })
    }

    // ---------------------------------------------------------------- mouse

    fn offset_at(&self, position: Point<Pixels>, cx: &App) -> Option<usize> {
        self.layout
            .as_ref()
            .map(|l| l.offset_for_position(self.buf(cx), self.scroll, position))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.hide_popovers(cx);
        let Some(offset) = self.offset_at(event.position, cx) else {
            return;
        };
        if event.modifiers.secondary() && event.click_count == 1 {
            // Cmd-click (Ctrl-click elsewhere): go to definition.
            self.select_range(offset..offset, cx);
            self.go_to_definition(&GoToDefinition, window, cx);
            return;
        }
        Perf::input_started(cx);
        self.document.update(cx, |d, _| d.seal_history());
        let (mode, origin) = match event.click_count {
            1 => (DragMode::Char, offset..offset),
            2 => (DragMode::Word, self.buf(cx).word_range_at(offset)),
            _ => (DragMode::Line, self.buf(cx).line_range_at(offset)),
        };
        if event.modifiers.shift && mode == DragMode::Char {
            let mut selections = self.selections.clone();
            selections[self.newest].head = offset;
            selections[self.newest].goal = SelectionGoal::None;
            self.set_selections(selections, self.newest);
        } else if event.modifiers.alt {
            let mut selections = self.selections.clone();
            selections.push(Selection::new(origin.start, origin.end));
            let newest = selections.len() - 1;
            self.set_selections(selections, newest);
        } else {
            self.set_selections(vec![Selection::new(origin.start, origin.end)], 0);
        }
        self.drag = Some(Drag { mode, origin });
        self.selections_changed(cx);
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.as_ref() else {
            if event.pressed_button.is_none() && !self.is_single_line() {
                self.hover_at(event.position, cx);
            }
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let Some(offset) = self.offset_at(event.position, cx) else {
            return;
        };
        let unit = match drag.mode {
            DragMode::Char => offset..offset,
            DragMode::Word => self.buf(cx).word_range_at(offset),
            DragMode::Line => self.buf(cx).line_range_at(offset),
        };
        let origin = drag.origin.clone();
        let selection = if unit.start < origin.start {
            Selection::new(origin.end, unit.start)
        } else {
            Selection::new(origin.start, unit.end.max(origin.end))
        };
        if self.selections[self.newest] == selection {
            return;
        }
        Perf::input_started(cx);
        let mut selections = self.selections.clone();
        selections[self.newest] = selection;
        self.set_selections(selections, self.newest);
        self.selections_changed(cx);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.drag = None;
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_height = self.layout.as_ref().map_or(px(20.), |l| l.line_height);
        let delta = event.delta.pixel_delta(line_height);
        self.scroll = point(self.scroll.x - delta.x, self.scroll.y - delta.y);
        if self.hover.is_some() {
            self.hover = None;
            self.hover_task = None;
        }
        // Clamped against content size during the next prepaint.
        cx.notify();
    }

    // ---------------------------------------------------------------- utf16

    fn offset_to_utf16(&self, offset: usize, cx: &App) -> usize {
        let rope = self.buf(cx).rope();
        rope.char_to_utf16_cu(rope.byte_to_char(self.buf(cx).clip_offset(offset)))
    }

    fn offset_from_utf16(&self, offset: usize, cx: &App) -> usize {
        let rope = self.buf(cx).rope();
        let offset = offset.min(rope.len_utf16_cu());
        rope.char_to_byte(rope.utf16_cu_to_char(offset))
    }

    fn range_to_utf16(&self, r: &Range<usize>, cx: &App) -> Range<usize> {
        self.offset_to_utf16(r.start, cx)..self.offset_to_utf16(r.end, cx)
    }

    fn range_from_utf16(&self, r: &Range<usize>, cx: &App) -> Range<usize> {
        self.offset_from_utf16(r.start, cx)..self.offset_from_utf16(r.end, cx)
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16, cx);
        actual_range.replace(self.range_to_utf16(&range, cx));
        Some(self.buf(cx).text_for_range(range))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let s = self.newest_selection();
        Some(UTF16Selection {
            range: self.range_to_utf16(&s.range(), cx),
            reversed: s.is_reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|r| self.range_to_utf16(r, cx))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let explicit = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r, cx))
            .or(self.marked_range.take());
        match explicit {
            // IME commit or a platform replacement: a single range.
            Some(range) => {
                let text = text.to_owned();
                self.set_selections(vec![Selection::new(range.start, range.end)], 0);
                self.edit_selections(cx, |_, s| (s.range(), text.clone(), text.len()));
            }
            None => self.handle_input(text, cx),
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r, cx))
            .or(self.marked_range.clone())
            .unwrap_or_else(|| self.newest_selection().range());
        let text_owned = text.to_owned();
        self.set_selections(vec![Selection::new(range.start, range.end)], 0);
        self.edit_selections(cx, |_, s| (s.range(), text_owned.clone(), text_owned.len()));
        self.marked_range = (!text.is_empty()).then(|| range.start..range.start + text.len());
        if let Some(sel) = new_selected_range_utf16 {
            // The IME's selection is relative to the marked text, in UTF-16.
            let base = self.offset_to_utf16(range.start, cx);
            let r = self.range_from_utf16(&(base + sel.start..base + sel.end), cx);
            self.set_selections(vec![Selection::new(r.start, r.end)], 0);
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: gpui::Bounds<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16, cx);
        self.layout
            .as_ref()?
            .bounds_for_offset(self.buf(cx), self.scroll, range.start)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.offset_at(point, cx)?;
        Some(self.offset_to_utf16(offset, cx))
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let single_line = self.is_single_line();
        div()
            .id("editor")
            .key_context(match (single_line, self.completion.is_some()) {
                (true, _) => "Editor mode=single_line",
                (false, true) => "Editor mode=full showing_completions",
                (false, false) => "Editor mode=full",
            })
            .track_focus(&self.focus_handle)
            .when(single_line, |d| {
                d.w_full()
                    .h(crate::settings::Settings::get(cx).line_height())
            })
            .when(!single_line, |d| d.size_full().bg(cx.theme().bg))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::move_word_left))
            .on_action(cx.listener(Self::move_word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::move_home))
            .on_action(cx.listener(Self::move_end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::move_to_start))
            .on_action(cx.listener(Self::move_to_end))
            .on_action(cx.listener(Self::select_to_start))
            .on_action(cx.listener(Self::select_to_end))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::indent))
            .on_action(cx.listener(Self::outdent))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::select_line))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::save_action))
            .on_action(cx.listener(Self::add_cursor_above))
            .on_action(cx.listener(Self::add_cursor_below))
            .on_action(cx.listener(Self::select_next_occurrence))
            .on_action(cx.listener(Self::move_line_up))
            .on_action(cx.listener(Self::move_line_down))
            .on_action(cx.listener(Self::duplicate_line))
            .on_action(cx.listener(Self::toggle_comment))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::show_completions))
            .on_action(cx.listener(Self::confirm_completion))
            .on_action(cx.listener(Self::select_next_completion))
            .on_action(cx.listener(Self::select_prev_completion))
            .on_action(cx.listener(Self::hide_completions))
            .on_action(cx.listener(Self::go_to_definition))
            .on_action(cx.listener(Self::find_references))
            .on_action(cx.listener(Self::rename_symbol))
            .on_action(cx.listener(Self::format_document))
            .on_action(cx.listener(Self::next_diagnostic))
            .on_action(cx.listener(Self::prev_diagnostic))
            .on_action(cx.listener(Self::show_hover))
            .on_action(cx.listener(Self::code_actions))
            .on_action(cx.listener(Self::stage_lines))
            .on_action(cx.listener(Self::revert_hunk))
            .on_action(cx.listener(Self::next_hunk))
            .on_action(cx.listener(Self::prev_hunk))
            .on_action(cx.listener(Self::accept_ours))
            .on_action(cx.listener(Self::accept_theirs))
            .on_action(cx.listener(Self::accept_both))
            .on_action(cx.listener(Self::next_conflict))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(EditorElement::new(cx.entity()))
            .children(self.render_completions(cx))
            .children(self.render_hover(cx))
            .children(self.render_signature(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use std::time::Instant;

    fn setup<'a>(
        cx: &'a mut TestAppContext,
        path: &str,
        text: &str,
    ) -> (Entity<Editor>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(Perf::new(Instant::now()));
            cx.set_global(Theme::dark());
            cx.set_global(crate::settings::Settings::default());
            bind_keys(cx);
        });
        let path = PathBuf::from(path);
        let text = text.to_owned();
        let (editor, cx) = cx.add_window_view(|_, cx| Editor::new(Some(path), &text, cx));
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx)));
        // Paint once so the input handler and layout snapshot exist.
        cx.run_until_parked();
        (editor, cx)
    }

    fn text(editor: &Entity<Editor>, cx: &mut VisualTestContext) -> String {
        cx.read(|cx| editor.read(cx).text(cx))
    }

    fn cursors(editor: &Entity<Editor>, cx: &mut VisualTestContext) -> Vec<Range<usize>> {
        cx.read(|cx| {
            editor
                .read(cx)
                .selections
                .iter()
                .map(|s| s.range())
                .collect()
        })
    }

    #[gpui::test]
    fn typing_and_undo(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.rs", "");
        cx.simulate_input("fn main() {");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("x");
        assert_eq!(text(&editor, cx), "fn main() {\n    x\n}");
        cx.simulate_keystrokes("secondary-z");
        assert_eq!(text(&editor, cx), "");
        cx.simulate_keystrokes("secondary-shift-z");
        assert_eq!(text(&editor, cx), "fn main() {\n    x\n}");
    }

    #[gpui::test]
    fn brackets_and_quotes_pair_up(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.ts", "");
        cx.simulate_input("f(");
        assert_eq!(text(&editor, cx), "f()");
        cx.simulate_input("\"a");
        assert_eq!(text(&editor, cx), "f(\"a\")");
        // Typing the closers steps over them instead of doubling.
        cx.simulate_input("\")");
        assert_eq!(text(&editor, cx), "f(\"a\")");
        assert_eq!(cursors(&editor, cx), vec![6..6]);
        // Backspace inside an empty pair removes both halves.
        cx.simulate_input("[");
        cx.simulate_keystrokes("backspace");
        assert_eq!(text(&editor, cx), "f(\"a\")");
        // A quote after a letter is an apostrophe, not a pair.
        cx.simulate_input(" don'");
        assert_eq!(text(&editor, cx), "f(\"a\") don'");
    }

    #[gpui::test]
    fn selection_wraps_in_pair(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.js", "value");
        cx.simulate_keystrokes("secondary-a");
        cx.simulate_input("(");
        assert_eq!(text(&editor, cx), "(value)");
    }

    #[gpui::test]
    fn single_line_mode_ignores_newlines(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Perf::new(Instant::now()));
            cx.set_global(Theme::dark());
            cx.set_global(crate::settings::Settings::default());
            bind_keys(cx);
        });
        let (editor, cx) = cx.add_window_view(|_, cx| Editor::single_line("Find", cx));
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx)));
        cx.run_until_parked();
        cx.simulate_input("a");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("b");
        assert_eq!(text(&editor, cx), "ab");
    }

    #[gpui::test]
    fn enter_between_brackets_splits_block(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.ts", "if (a) {}");
        cx.simulate_keystrokes("end left enter");
        assert_eq!(text(&editor, cx), "if (a) {\n  \n}".replace("  ", "    "));
        assert_eq!(cursors(&editor, cx), vec![13..13]);
    }

    #[gpui::test]
    fn multi_cursor_editing(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.txt", "one\ntwo\nthree");
        cx.simulate_keystrokes("secondary-alt-down secondary-alt-down");
        assert_eq!(cursors(&editor, cx).len(), 3);
        cx.simulate_input("- ");
        assert_eq!(text(&editor, cx), "- one\n- two\n- three");
        cx.simulate_keystrokes("backspace backspace");
        assert_eq!(text(&editor, cx), "one\ntwo\nthree");
    }

    #[gpui::test]
    fn select_next_occurrence_and_replace(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.js", "let foo = foo + foo;");
        cx.simulate_keystrokes("right right right right secondary-d secondary-d secondary-d");
        assert_eq!(cursors(&editor, cx), vec![4..7, 10..13, 16..19]);
        cx.simulate_input("bar");
        assert_eq!(text(&editor, cx), "let bar = bar + bar;");
    }

    #[gpui::test]
    fn move_duplicate_and_comment_lines(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.rs", "a\nb\nc");
        cx.simulate_keystrokes("down alt-down");
        assert_eq!(text(&editor, cx), "a\nc\nb");
        cx.simulate_keystrokes("alt-up alt-up");
        assert_eq!(text(&editor, cx), "b\na\nc");
        cx.simulate_keystrokes("alt-shift-down");
        assert_eq!(text(&editor, cx), "b\nb\na\nc");
        cx.simulate_keystrokes("secondary-/");
        assert_eq!(text(&editor, cx), "b\n// b\na\nc");
        cx.simulate_keystrokes("secondary-/");
        assert_eq!(text(&editor, cx), "b\nb\na\nc");
    }

    #[gpui::test]
    fn indent_and_outdent_selection(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.py", "a\nb\n");
        cx.simulate_keystrokes("shift-down shift-down tab");
        assert_eq!(text(&editor, cx), "    a\n    b\n");
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(text(&editor, cx), "a\nb\n");
    }

    #[gpui::test]
    fn backspace_removes_one_indent_level(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.rs", "        x");
        cx.simulate_keystrokes("end home backspace");
        assert_eq!(text(&editor, cx), "    x");
    }

    #[gpui::test]
    fn copy_without_selection_copies_line(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.txt", "first\nsecond");
        cx.simulate_keystrokes("secondary-c down secondary-v");
        assert_eq!(text(&editor, cx), "first\nfirst\nsecond");
        let _ = editor;
    }

    #[gpui::test]
    fn highlights_follow_edits(cx: &mut TestAppContext) {
        let (editor, cx) = setup(cx, "a.rs", "");
        cx.simulate_input("let s = \"hi\";");
        cx.run_until_parked();
        let spans = cx.read(|cx| {
            let e = editor.read(cx);
            e.doc(cx)
                .syntax()
                .unwrap()
                .highlights(e.rope(cx), 0..e.buf(cx).len())
        });
        assert!(
            spans
                .iter()
                .any(|(r, k)| *r == (8..12) && *k == HighlightKind::String)
        );
    }
}
