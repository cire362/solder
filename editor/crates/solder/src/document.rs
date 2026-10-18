//! A file's contents and everything derived from them: syntax tree, undo
//! history, dirty state. Editors are views onto a document, so the same file
//! can be open in two panes and edits show up in both.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{FutureExt, future::Shared};
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
#[cfg(not(test))]
const REPARSE_BUDGET: Duration = Duration::from_millis(1);
/// In tests the tree is waited for. What a key does to a line's indent is
/// decided with the tree of that moment, and a runner busy with three
/// hundred tests does not parse in a millisecond: a test of what a
/// language says about typing then failed for how fast it was said.
#[cfg(test)]
const REPARSE_BUDGET: Duration = Duration::from_secs(10);
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

/// Something a language server offers to do with a line, said at the
/// line's end and done by a click: `Run`, `3 references`. `offset` is in
/// the line it is of.
#[derive(Clone, Debug, PartialEq)]
pub struct Lens {
    pub offset: usize,
    pub title: String,
    /// The server that offered it, which is the one to run it, and how
    /// that server counts the places it names.
    pub server: &'static str,
    pub encoding: lsp::Encoding,
    pub command: String,
    pub arguments: Vec<serde_json::Value>,
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
    /// A tree is in step with the text again, made after a key was
    /// answered without one.
    SyntaxChanged,
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

/// A file larger than this is opened to be read and not edited: every
/// edit of it would be felt.
pub const EDIT_LIMIT: u64 = 64 * 1024 * 1024;
/// And one larger than this is not read at all.
pub const READ_LIMIT: u64 = 512 * 1024 * 1024;

/// A file as it is opened: its text, what it is written in, and why it
/// is only to be read, where it is.
pub struct Loaded {
    pub text: String,
    pub encoding: text::encoding::Encoding,
    pub read_only: Option<ReadOnly>,
    pub stamp: Option<crate::remote::Stamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadOnly {
    Binary,
    Large(u64),
    TooLarge(u64),
}

impl ReadOnly {
    pub fn message(&self) -> String {
        match self {
            Self::Binary => crate::lang::t(
                "This is not a text file. It is shown as it is and cannot be edited.",
            )
            .into(),
            Self::Large(bytes) => crate::lang::tf(
                "This file is {}: too large to edit. It is open for reading.",
                &[&megabytes(*bytes)],
            ),
            Self::TooLarge(bytes) => crate::lang::tf(
                "This file is {}: too large to open here.",
                &[&megabytes(*bytes)],
            ),
        }
    }
}

enum SaveFailure {
    Encoding(char, text::encoding::Encoding),
    Io(std::io::Error),
    PathChanged,
    RecoveryRevisionMissing,
}

impl SaveFailure {
    fn message(&self) -> String {
        match self {
            Self::Encoding(character, encoding) => crate::lang::tf(
                "Not saved: {} cannot be written in {}. Save with another encoding.",
                &[&format!("{character:?}"), &encoding.name()],
            ),
            Self::Io(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                crate::lang::t("The file changed on the target. Reopen it before saving.").into()
            }
            Self::Io(error) => crate::lang::tf("Not saved: {}", &[&error]),
            Self::PathChanged => {
                crate::lang::t("The file path changed during save. Save again.").into()
            }
            Self::RecoveryRevisionMissing => {
                crate::lang::t("This recovered text has no target revision. Save it to a new file.")
                    .into()
            }
        }
    }
}

impl Loaded {
    /// The text of bytes, in the encoding they are found to be in.
    pub fn of(bytes: &[u8]) -> Self {
        Self::within(bytes, EDIT_LIMIT)
    }

    /// The same, with what is too large to edit said by the caller.
    pub fn within(bytes: &[u8], edit_limit: u64) -> Self {
        let decoded = text::encoding::decode(bytes);
        let size = bytes.len() as u64;
        let read_only = if decoded.binary {
            Some(ReadOnly::Binary)
        } else if size > edit_limit {
            Some(ReadOnly::Large(size))
        } else {
            None
        };
        Self {
            text: decoded.text,
            encoding: decoded.encoding,
            read_only,
            stamp: None,
        }
    }
}

fn megabytes(bytes: u64) -> String {
    match bytes {
        0..1_048_576 => crate::lang::tf("{} KB", &[&bytes.div_ceil(1024)]),
        _ => crate::lang::tf("{} MB", &[&(bytes / 1_048_576)]),
    }
}

/// Reads a file to be opened in an editor. Blocking.
pub fn load(path: &Path) -> std::io::Result<Loaded> {
    let size = crate::remote::metadata(path)?.len();
    if size > READ_LIMIT {
        return Ok(Loaded {
            text: String::new(),
            encoding: Default::default(),
            read_only: Some(ReadOnly::TooLarge(size)),
            stamp: None,
        });
    }
    let (bytes, stamp) = crate::remote::read_versioned(path)?;
    let mut loaded = Loaded::of(&bytes);
    loaded.stamp = stamp;
    Ok(loaded)
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
    pending_saves: usize,
    /// How long a key waits for the tree: `REPARSE_BUDGET`, or none at all
    /// in a test of what happens when the tree comes later.
    reparse_budget: Duration,
    /// Foreground completion, including the revision and dirty-state update.
    save_tail: Option<Shared<Task<bool>>>,
    /// Sorted by start.
    diagnostics: Arc<Vec<Diagnostic>>,
    /// Sorted by where they are.
    server_inlays: Arc<Vec<Inlay>>,
    inlays: Arc<Vec<Inlay>>,
    /// Sorted by where they are.
    lenses: Arc<Vec<Lens>>,
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
    blame_repo: Option<crate::git::Repo>,
    blame: Arc<Vec<crate::git_history::Blame>>,
    blame_error: Option<gpui::SharedString>,
    blame_task: Option<Task<()>>,
    read_only: bool,
    /// What the file is written in, which is what it is saved in.
    encoding: text::encoding::Encoding,
    /// The revision this view read, independent of reads made by other views.
    stamp: Option<crate::remote::Stamp>,
    /// Undo may return to today's target text, which is not proof that
    /// the recovered work was saved. Keep its original until an answer.
    recovery_source: Option<RecoverySource>,
    /// Something to say above the text: why it cannot be edited, or why
    /// it was not saved.
    notice: Option<gpui::SharedString>,
    /// Title for documents without a path (a conflict side, a diff base).
    title_override: Option<String>,
    /// Picks the grammar when there is no path.
    language_path: Option<PathBuf>,
}

struct RecoverySource {
    path: PathBuf,
    stamp: Option<crate::remote::Stamp>,
    original: text::Rope,
    baseline: text::Rope,
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
            pending_saves: 0,
            reparse_budget: REPARSE_BUDGET,
            save_tail: None,
            diagnostics: Arc::default(),
            server_inlays: Arc::default(),
            inlays: Arc::default(),
            lenses: Arc::default(),
            decorations: Default::default(),
            decoration_ranges: Vec::new(),
            semantic: Arc::default(),
            semantic_generation: 0,
            diff_base: None,
            hunks: Arc::default(),
            conflicts: Arc::default(),
            git_task: None,
            blame_repo: None,
            blame: Arc::default(),
            blame_error: None,
            blame_task: None,
            read_only: false,
            encoding: Default::default(),
            stamp: None,
            recovery_source: None,
            notice: None,
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

    /// The text of a cell of a notebook: no file of its own, and written
    /// in the language `language_path` would be.
    pub fn cell(language_path: PathBuf, content: &str, cx: &mut Context<Self>) -> Self {
        let mut doc = Self::new(None, content, cx);
        doc.language_path = Some(language_path);
        doc.initial_parse(cx);
        doc
    }

    /// Restoration freezes a cell until its host and UI agree on the model.
    pub(crate) fn lock_cell(&mut self, locked: bool, cx: &mut Context<Self>) {
        debug_assert!(self.path.is_none() && self.title_override.is_none());
        self.read_only = locked;
        cx.notify();
    }

    /// A file as it was read from disk.
    pub fn open(path: PathBuf, loaded: &Loaded, cx: &mut Context<Self>) -> Self {
        let mut doc = Self::new(Some(path), &loaded.text, cx);
        doc.take(loaded);
        doc
    }

    fn take(&mut self, loaded: &Loaded) {
        self.encoding = loaded.encoding;
        self.stamp = loaded.stamp.clone();
        self.read_only = loaded.read_only.is_some();
        self.notice = loaded.read_only.as_ref().map(|why| why.message().into());
    }

    /// Reads the file again as written in another encoding: what is on
    /// disk, in place of the text here.
    pub fn reopen(
        &mut self,
        bytes: &[u8],
        stamp: Option<crate::remote::Stamp>,
        encoding: text::encoding::Encoding,
        cx: &mut Context<Self>,
    ) {
        let text = text::encoding::decode_as(bytes, encoding);
        self.reload(&text, cx);
        // Reopening an encoding retains undo history. Its later undo
        // must not authorize the recovered text against a newer target.
        if self.recovery_source.is_none() {
            self.stamp = stamp;
        }
        self.encoding = encoding;
        // What could not be saved in the old one is no longer true.
        if !self.read_only {
            self.notice = None;
        }
        cx.notify();
    }

    pub fn encoding(&self) -> text::encoding::Encoding {
        self.encoding
    }

    /// Changes what the file is saved in. The text stays; the file on
    /// disk is then not what a save would write.
    pub fn set_encoding(&mut self, encoding: text::encoding::Encoding, cx: &mut Context<Self>) {
        if self.encoding != encoding && !self.read_only {
            self.encoding = encoding;
            self.text.touch();
            self.update_dirty(cx);
            cx.notify();
        }
    }

    pub fn line_ending(&self) -> text::LineEnding {
        self.text.line_ending()
    }

    pub fn set_line_ending(&mut self, ending: text::LineEnding, cx: &mut Context<Self>) {
        if !self.read_only {
            self.text.set_line_ending(ending);
            self.update_dirty(cx);
            cx.notify();
        }
    }

    /// What is to be said above the text, if anything.
    pub fn notice(&self) -> Option<&gpui::SharedString> {
        self.notice.as_ref().or(self.blame_error.as_ref())
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

    pub fn blame_enabled(&self) -> bool {
        self.blame_repo.is_some()
    }

    pub fn blame(&self) -> &[crate::git_history::Blame] {
        &self.blame
    }

    pub fn toggle_blame(&mut self, repo: crate::git::Repo, cx: &mut Context<Self>) {
        self.blame_repo = if self.blame_enabled() {
            None
        } else {
            Some(repo)
        };
        self.blame = Arc::default();
        self.blame_error = None;
        self.blame_task = None;
        self.refresh_blame(cx);
        cx.emit(DocumentEvent::GitChanged);
    }

    pub fn refresh_blame(&mut self, cx: &mut Context<Self>) {
        let (Some(repo), Some(path)) = (self.blame_repo.clone(), self.path.clone()) else {
            return;
        };
        let Some(file) = repo.relative(&path) else {
            return;
        };
        let rope = self.text.rope().clone();
        let version = self.version();
        self.blame_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let result = cx
                .background_executor()
                .spawn(async move { repo.blame(&file, &rope.to_string()) })
                .await;
            this.update(cx, |this, cx| {
                if this.version() != version {
                    return;
                }
                match result {
                    Ok(rows) => {
                        this.blame = Arc::new(rows);
                        this.blame_error = None;
                    }
                    Err(e) => {
                        this.blame = Arc::default();
                        this.blame_error = Some(format!("Blame: {}", e.0).into());
                    }
                }
                cx.emit(DocumentEvent::GitChanged);
                cx.notify();
            })
            .ok();
        }));
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
        self.text.is_dirty() || self.recovery_source.is_some() || self.is_saving()
    }

    pub(crate) fn is_saving(&self) -> bool {
        self.pending_saves != 0
    }

    /// Await this tail, then recapture it: another save may have been queued.
    pub(crate) fn pending_save(&self) -> Option<Shared<Task<bool>>> {
        self.is_saving().then(|| self.save_tail.clone()).flatten()
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
            "\t" => crate::lang::t("Tabs"),
            "  " => crate::lang::t("Spaces: 2"),
            _ => crate::lang::t("Spaces: 4"),
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

    pub fn lenses(&self) -> &Arc<Vec<Lens>> {
        &self.lenses
    }

    pub fn set_lenses(&mut self, mut lenses: Vec<Lens>, cx: &mut Context<Self>) {
        lenses.retain(|lens| !lens.title.trim().is_empty());
        lenses.sort_by_key(|lens| lens.offset);
        if *self.lenses != lenses {
            self.lenses = Arc::new(lenses);
            cx.emit(DocumentEvent::HintsChanged);
            cx.notify();
        }
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
        if self.read_only {
            return None;
        }
        let (edits, selections) = self.text.undo()?;
        self.after_change(edits.into(), Some(origin), cx);
        Some(selections)
    }

    pub fn redo(&mut self, origin: EntityId, cx: &mut Context<Self>) -> Option<Vec<Selection>> {
        if self.read_only {
            return None;
        }
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
        if !self.lenses.is_empty() {
            let moved = self
                .lenses
                .iter()
                .map(|lens| Lens {
                    offset: map_offset(lens.offset, &edits),
                    ..lens.clone()
                })
                .collect();
            self.lenses = Arc::new(moved);
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
        if self.blame_enabled() {
            // Old row numbers could name a different author after an edit.
            self.blame = Arc::default();
            self.refresh_blame(cx);
        }
        self.update_dirty(cx);
    }

    fn update_dirty(&mut self, cx: &mut Context<Self>) {
        if self.is_dirty() != self.was_dirty {
            self.was_dirty = self.is_dirty();
            cx.emit(DocumentEvent::DirtyChanged);
        }
    }

    // ------------------------------------------------------------ files

    /// Writes the text to its path. Resolves to whether the write succeeded.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        let Some(path) = self.path.clone() else {
            return Task::ready(false);
        };
        if self.read_only {
            return Task::ready(false);
        }
        let checkpoint = self.text.save_checkpoint();
        let rope = self.text.rope().clone();
        let line_ending = self.text.line_ending();
        let encoding = self.encoding;
        let saved_path = path.clone();
        let previous = self.save_tail.take();
        self.pending_saves += 1;
        self.was_dirty = self.is_dirty();
        cx.emit(DocumentEvent::DirtyChanged);
        let save = cx
            .spawn(async move |this, cx| {
                if let Some(previous) = previous {
                    // Each request reports its own result. A failed write
                    // leaves the revision available for the next attempt.
                    previous.await;
                }
                // Read our revision after the previous foreground completion,
                // never one captured before it wrote, or from a different path.
                let Ok(stamp) = this.read_with(cx, |this, _| {
                    if this.path.as_ref() != Some(&saved_path) {
                        return Err(SaveFailure::PathChanged);
                    }
                    let source = this
                        .recovery_source
                        .as_ref()
                        .filter(|recovered| recovered.path == saved_path);
                    let stamp = source
                        .map_or_else(|| this.stamp.clone(), |recovered| recovered.stamp.clone());
                    if source.is_some() && stamp.is_none() {
                        return Err(SaveFailure::RecoveryRevisionMissing);
                    }
                    Ok(stamp)
                }) else {
                    return false;
                };
                let result = match stamp {
                    Ok(stamp) => {
                        // Flattening and restoring line endings can be large,
                        // so they run with the encoding and IO off this thread.
                        cx.background_executor()
                            .spawn(async move {
                                let text = match line_ending {
                                    text::LineEnding::Lf => rope.to_string(),
                                    text::LineEnding::CrLf => {
                                        rope.to_string().replace('\n', "\r\n")
                                    }
                                };
                                let bytes = text::encoding::encode(&text, encoding).map_err(
                                    |character| SaveFailure::Encoding(character, encoding),
                                )?;
                                crate::remote::write_versioned(&path, &bytes, stamp.as_ref())
                                    .map_err(SaveFailure::Io)
                            })
                            .await
                    }
                    Err(why) => Err(why),
                };
                this.update(cx, |this, cx| {
                    this.pending_saves -= 1;
                    let result = if this.path.as_ref() == Some(&saved_path) {
                        result
                    } else {
                        Err(SaveFailure::PathChanged)
                    };
                    let saved = match result {
                        Ok(stamp) => {
                            this.stamp = stamp;
                            // Undo during IO may return to the old clean state.
                            // Only the snapshot actually written is now saved.
                            this.text.mark_saved_at(checkpoint);
                            this.recovery_source = None;
                            this.notice = None;
                            true
                        }
                        Err(why) => {
                            // The language may have changed while the save ran.
                            this.notice = Some(why.message().into());
                            false
                        }
                    };
                    this.was_dirty = this.is_dirty();
                    // Pending state changes even when the text stays dirty.
                    cx.emit(DocumentEvent::DirtyChanged);
                    if saved {
                        cx.emit(DocumentEvent::Saved);
                    }
                    saved
                })
                .unwrap_or(false)
            })
            .shared();
        self.save_tail = Some(save.clone());
        // The document owns the write; cancelling a caller's wait cannot cancel it.
        cx.spawn(async move |_, _| save.await)
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

    /// Puts back text that was typed and never saved, as changes that are
    /// not saved: one step back from it is the file as it is on disk.
    /// Named recovery retains its own read revision even if the text
    /// already matches today's file. None means an old, unversioned copy.
    pub(crate) fn recover(
        &mut self,
        text: &str,
        stamp: Option<crate::remote::Stamp>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.read_only {
            return false;
        }
        let named = self.path.is_some();
        if let Some(path) = self.path.clone() {
            // The startup read describes today's bytes, not the revision
            // against which the recovered edits were originally made.
            // Length and timestamp alone cannot prove a recovered source's
            // identity after an external edit which preserves both.
            self.stamp = stamp.filter(|stamp| {
                stamp.hash.as_ref().is_some_and(|hash| {
                    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            });
            self.recovery_source = Some(RecoverySource {
                path,
                stamp: self.stamp.clone(),
                original: text::Rope::from_str(text),
                baseline: self.text.rope().clone(),
            });
            self.notice = Some(if self.stamp.is_none() {
                crate::lang::t("This recovered text has no target revision. Save it to a new file.")
                    .into()
            } else {
                crate::lang::t(
                    "This text was recovered. Its recovery copy is kept until you save or close it.",
                )
                .into()
            });
        }
        if *self.text.rope() == text {
            self.update_dirty(cx);
            cx.notify();
            return named;
        }
        let len = self.text.len();
        self.text.seal_history();
        let applied: Arc<[text::Edit]> = self
            .text
            .edit([(0..len, text.to_string())], &[], Instant::now())
            .into();
        self.text.seal_history();
        if !named {
            let said = crate::lang::t(
                "This text was not saved when Solder closed. Undo brings back the file as it is on disk.",
            );
            self.notice = Some(said.into());
        }
        self.after_change(applied, None, cx);
        true
    }

    /// An undo to the startup baseline must not replace the only
    /// recovery copy of the original work with the target's newer text.
    pub(crate) fn recovery_snapshot(
        &self,
    ) -> (Option<PathBuf>, text::Rope, Option<crate::remote::Stamp>) {
        match &self.recovery_source {
            Some(recovered) => {
                let text = if self.text.rope() == &recovered.baseline {
                    &recovered.original
                } else {
                    self.text.rope()
                };
                // An attempted Save As does not reassign the original
                // recovery to its destination before the write succeeds.
                (
                    Some(recovered.path.clone()),
                    text.clone(),
                    recovered.stamp.clone(),
                )
            }
            None => (
                self.path.clone(),
                self.text.rope().clone(),
                self.stamp.clone(),
            ),
        }
    }

    pub(crate) fn protected_save_source(&self) -> Option<PathBuf> {
        self.recovery_source
            .as_ref()
            .map(|recovered| recovered.path.clone())
    }

    pub(crate) fn save_failure(&mut self, error: std::io::Error, cx: &mut Context<Self>) {
        self.notice = Some(SaveFailure::Io(error).message().into());
        cx.notify();
    }

    /// A selected destination may be an alias of the source. That changes
    /// its displayed path, not the revision which permits replacing it.
    pub(crate) fn set_save_target(
        &mut self,
        path: PathBuf,
        destination: Option<crate::remote::Stamp>,
        same_current: bool,
        same_recovery: bool,
        cx: &mut Context<Self>,
    ) {
        if self.path.as_ref() == Some(&path) {
            return;
        }
        let stamp = if same_recovery {
            self.recovery_source
                .as_ref()
                .and_then(|recovered| recovered.stamp.clone())
        } else if same_current {
            self.stamp.clone()
        } else {
            destination
        };
        self.set_path(path.clone(), cx);
        self.stamp = stamp;
        if same_recovery && let Some(recovered) = self.recovery_source.as_mut() {
            recovered.path = path;
        }
    }

    pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.path.as_ref() == Some(&path) {
            return;
        }
        // The old clean checkpoint belongs to another destination. Undo
        // cannot make this one saved, even if its first write fails.
        self.text.touch();
        self.was_dirty = self.is_dirty();
        cx.emit(DocumentEvent::DirtyChanged);
        self.stamp = None;
        // Choosing a destination is not a successful save. In particular,
        // an undo to the baseline still needs its protected original.
        self.changed_path(path, cx);
    }

    fn changed_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.path = Some(path);
        self.blame = Arc::default();
        self.blame_error = None;
        self.refresh_blame(cx);
        self.syntax = None;
        self.syntax_generation += 1;
        self.initial_parse(cx);
        self.update_dirty(cx);
        cx.emit(DocumentEvent::PathChanged);
    }

    /// A rename keeps the revision that was opened; Save As does not.
    pub fn renamed(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(recovered) = self.recovery_source.as_mut()
            && self.path.as_ref() == Some(&recovered.path)
        {
            recovered.path = path.clone();
            // Recovery must write its changed source path even though a
            // rename did not change the text's contents.
            self.text.touch();
        }
        self.changed_path(path, cx);
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

    /// Whether a key waits for the tree. Without, it is always made later.
    #[cfg(test)]
    pub fn wait_for_the_tree(&mut self, wait: bool) {
        self.reparse_budget = if wait { REPARSE_BUDGET } else { Duration::ZERO };
    }

    fn set_syntax(&mut self, tree: Option<SyntaxTree>, cx: &mut Context<Self>) {
        self.syntax = tree;
        self.syntax_generation += 1;
        cx.emit(DocumentEvent::SyntaxChanged);
        cx.notify();
    }

    fn reparse(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = self.syntax.as_mut() else {
            return;
        };
        self.syntax_generation += 1;
        if tree.reparse_within(self.text.rope(), self.reparse_budget) {
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
    use gpui::AppContext;

    async fn load_off_ui(path: &Path, cx: &gpui::TestAppContext) -> Loaded {
        let path = path.to_path_buf();
        cx.background_spawn(async move { load(&path).unwrap() })
            .await
    }

    async fn read_off_ui(path: &Path, cx: &gpui::TestAppContext) -> Vec<u8> {
        let path = path.to_path_buf();
        cx.background_spawn(async move { std::fs::read(path).unwrap() })
            .await
    }

    fn hold_save_queue(
        doc: &mut Document,
        cx: &mut Context<Document>,
    ) -> futures::channel::oneshot::Sender<()> {
        let (release, held) = futures::channel::oneshot::channel();
        let previous = doc.save_tail.take();
        doc.save_tail = Some(
            cx.spawn(async move |_, _| {
                if let Some(previous) = previous {
                    previous.await;
                }
                held.await.is_ok()
            })
            .shared(),
        );
        release
    }

    #[gpui::test]
    fn file_notices_use_the_language_when_the_file_is_shown(cx: &mut gpui::TestAppContext) {
        let loaded = std::thread::spawn(|| Loaded::of(b"\0\x01\x02"))
            .join()
            .unwrap();
        crate::lang::set(crate::lang::Language::Russian);
        let doc = cx.new(|cx| Document::open(PathBuf::from("data.bin"), &loaded, cx));
        assert_eq!(
            doc.read_with(cx, |doc, _| doc.notice().unwrap().to_string()),
            crate::lang::t("This is not a text file. It is shown as it is and cannot be edited.")
        );
        assert!(ReadOnly::Large(2048).message().contains("2 КБ"));
        let failure = SaveFailure::Io(std::io::Error::other("disk detail"));
        assert_eq!(failure.message(), "Не сохранено: disk detail");
        crate::lang::set(crate::lang::Language::English);
        assert_eq!(failure.message(), "Not saved: disk detail");
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn remote_documents_keep_their_own_revisions_and_encoding(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let directory = db::testing::dir("document-remote").canonicalize().unwrap();
        let source = directory.join("server");
        let cache = directory.join("cache");
        std::fs::create_dir(&source).unwrap();
        let file = source.join("file.txt");
        std::fs::write(&file, [0xff, 0xfe, b'A', 0, b'\n', 0]).unwrap();
        let mount = crate::remote::fixture(&source, &cache);
        let path = cache.join("file.txt");
        let loaded = load(&path).unwrap();
        let first = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        // Another reader must not replace the revision the first document owns.
        std::fs::write(&file, [0xff, 0xfe, b'B', 0, b'\n', 0]).unwrap();
        let next = load(&path).unwrap();
        let second = cx.new(|cx| Document::open(path.clone(), &next, cx));
        first.update(cx, |doc, cx| {
            doc.apply_edits(vec![(0..1, "C".into())], None, cx)
        });
        assert!(!first.update(cx, |doc, cx| doc.save(cx)).await);
        assert!(first.read_with(cx, |doc, _| doc.is_dirty() && doc.notice().is_some()));
        assert_eq!(
            std::fs::read(&file).unwrap(),
            [0xff, 0xfe, b'B', 0, b'\n', 0]
        );
        second.update(cx, |doc, cx| {
            doc.apply_edits(vec![(0..1, "D".into())], None, cx)
        });
        assert!(second.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(
            std::fs::read(&file).unwrap(),
            [0xff, 0xfe, b'D', 0, b'\n', 0]
        );
        crate::remote::rename(&path, &cache.join("renamed.txt")).unwrap();
        second.update(cx, |doc, cx| doc.renamed(cache.join("renamed.txt"), cx));
        second.update(cx, |doc, cx| {
            doc.apply_edits(vec![(0..1, "E".into())], None, cx)
        });
        assert!(second.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(
            std::fs::read(source.join("renamed.txt")).unwrap(),
            [0xff, 0xfe, b'E', 0, b'\n', 0]
        );
        drop(mount);
        second.update(cx, |doc, cx| {
            doc.apply_edits(vec![(0..1, "F".into())], None, cx)
        });
        assert!(!second.update(cx, |doc, cx| doc.save(cx)).await);
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn remote_recovery_preserves_originals_through_same_content_undo_and_reopen(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let directory = db::testing::dir("document-recovery-originals")
            .canonicalize()
            .unwrap();
        let source = directory.join("server");
        let cache = directory.join("cache");
        std::fs::create_dir(&source).unwrap();
        let target = source.join("file.txt");
        let path = cache.join("file.txt");
        std::fs::write(&target, b"before").unwrap();
        let _mount = crate::remote::fixture(&source, &cache);
        let original = load(&path).unwrap().stamp;
        std::fs::write(&target, b"today").unwrap();
        let loaded = load(&path).unwrap();

        let same = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        assert!(same.update(cx, |doc, cx| doc.recover("today", original.clone(), cx)));
        assert!(same.read_with(cx, |doc, _| doc.is_dirty()));
        assert_eq!(
            same.read_with(cx, |doc, _| doc.recovery_snapshot().2),
            original
        );
        assert!(!same.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(std::fs::read(&target).unwrap(), b"today");

        let restored = cx.new(|cx| Document::open(path, &loaded, cx));
        restored.update(cx, |doc, cx| doc.recover("unsaved", original.clone(), cx));
        let origin = restored.entity_id();
        restored.update(cx, |doc, cx| doc.undo(origin, cx)).unwrap();
        assert_eq!(
            restored.read_with(cx, |doc, _| doc.text().rope().to_string()),
            "today"
        );
        assert!(restored.read_with(cx, |doc, _| doc.is_dirty()));
        let (_, text, stamp) = restored.read_with(cx, |doc, _| doc.recovery_snapshot());
        assert_eq!(text.to_string(), "unsaved");
        assert_eq!(stamp, original);
        restored.update(cx, |doc, cx| doc.redo(origin, cx)).unwrap();
        // An encoding reread is undoable too. It cannot borrow today's
        // revision and then save the old recovery through another undo.
        restored.update(cx, |doc, cx| {
            doc.reopen(b"today", loaded.stamp.clone(), loaded.encoding, cx);
            doc.undo(origin, cx).unwrap();
        });
        assert_eq!(
            restored.read_with(cx, |doc, _| doc.recovery_snapshot().2),
            original
        );
        assert!(!restored.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(std::fs::read(&target).unwrap(), b"today");
    }

    #[gpui::test]
    async fn a_local_document_refuses_unknown_or_external_revisions(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let path = db::testing::dir("document-local-revisions").join("file.txt");
        std::fs::write(&path, "before").unwrap();
        let unknown = cx.new(|cx| Document::new(Some(path.clone()), "ours", cx));
        assert!(!unknown.update(cx, |d, cx| d.save(cx)).await);
        assert_eq!(std::fs::read(&path).unwrap(), b"before");
        let loaded = load(&path).unwrap();
        let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        document.update(cx, |d, cx| {
            d.apply_edits(vec![(0..6, "ours".into())], None, cx)
        });
        std::fs::write(&path, "external").unwrap();
        assert!(!document.update(cx, |d, cx| d.save(cx)).await);
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
        document.read_with(cx, |d, _| {
            assert!(d.is_dirty());
            assert_eq!(d.stamp, loaded.stamp);
            assert!(d.notice().is_some());
        });
    }

    #[gpui::test]
    async fn local_recovery_checks_the_original_length_time_and_hash(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        for (change, bytes) in [
            ("length", "longer than before"),
            ("time", "before"),
            ("hash", "edited"),
        ] {
            let (path, original, loaded) = cx
                .background_spawn(async move {
                    let path = db::testing::dir("document-local-recovery-revision")
                        .join(format!("{change}.txt"));
                    std::fs::write(&path, "before").unwrap();
                    let original = load(&path).unwrap().stamp.unwrap();
                    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                    std::fs::write(&path, bytes).unwrap();
                    let modified = if change == "time" {
                        modified + Duration::from_secs(1)
                    } else {
                        modified
                    };
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&path)
                        .unwrap()
                        .set_times(std::fs::FileTimes::new().set_modified(modified))
                        .unwrap();
                    let loaded = load(&path).unwrap();
                    let current = loaded.stamp.as_ref().unwrap();
                    assert_eq!(original.length == current.length, change != "length");
                    assert_eq!(original.mtime == current.mtime, change != "time");
                    assert_eq!(original.hash == current.hash, change == "time");
                    (path, original, loaded)
                })
                .await;
            let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
            assert!(document.update(cx, |doc, cx| {
                doc.recover("unsaved", Some(original.clone()), cx)
            }));
            assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
            assert_eq!(read_off_ui(&path, cx).await, bytes.as_bytes());
            document.read_with(cx, |doc, _| {
                assert!(doc.is_dirty());
                assert!(!doc.is_saving());
                assert!(doc.notice().is_some());
                assert_eq!(doc.stamp, Some(original.clone()));
                let snapshot = doc.recovery_snapshot();
                assert_eq!(snapshot.0, Some(path.clone()));
                assert_eq!(snapshot.1.to_string(), "unsaved");
                assert_eq!(snapshot.2, Some(original));
            });
        }
    }

    #[gpui::test]
    async fn local_recovery_stays_dirty_at_the_startup_baseline_and_after_reopen(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let (path, original, loaded) = cx
            .background_spawn(async move {
                let path = db::testing::dir("document-local-recovery-baseline").join("file.txt");
                std::fs::write(&path, "before").unwrap();
                let original = load(&path).unwrap().stamp;
                std::fs::write(&path, "today").unwrap();
                let loaded = load(&path).unwrap();
                (path, original, loaded)
            })
            .await;
        let same = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        assert!(same.update(cx, |doc, cx| doc.recover("today", original.clone(), cx)));
        same.read_with(cx, |doc, _| {
            assert!(!doc.text.is_dirty());
            assert!(doc.is_dirty());
            assert_eq!(doc.recovery_snapshot().2, original);
        });
        assert!(!same.update(cx, |doc, cx| doc.save(cx)).await);

        let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        let origin = document.entity_id();
        document.update(cx, |doc, cx| {
            assert!(doc.recover("unsaved", original.clone(), cx));
            doc.undo(origin, cx).unwrap();
            assert_eq!(doc.text.rope().to_string(), "today");
            assert!(!doc.text.is_dirty());
            assert!(doc.is_dirty());
            assert_eq!(doc.recovery_snapshot().1.to_string(), "unsaved");
            doc.redo(origin, cx).unwrap();
            doc.reopen(b"today", loaded.stamp.clone(), loaded.encoding, cx);
            assert!(doc.is_dirty());
            assert_eq!(doc.recovery_snapshot().1.to_string(), "unsaved");
            doc.undo(origin, cx).unwrap();
            assert_eq!(doc.text.rope().to_string(), "unsaved");
            assert_eq!(doc.stamp, original);
            assert_eq!(doc.recovery_snapshot().2, original);
        });
        assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(read_off_ui(&path, cx).await, b"today");
    }

    #[gpui::test]
    async fn unchanged_local_recovery_saves_and_advances_its_revision(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        for recovered in ["before", "unsaved"] {
            let (path, loaded) = cx
                .background_spawn(async move {
                    let path = db::testing::dir("document-local-recovery-save")
                        .join(format!("{recovered}.txt"));
                    std::fs::write(&path, "before").unwrap();
                    let loaded = load(&path).unwrap();
                    (path, loaded)
                })
                .await;
            let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
            assert!(document.update(cx, |doc, cx| {
                doc.recover(recovered, loaded.stamp.clone(), cx)
            }));
            assert!(document.read_with(cx, |doc, _| doc.is_dirty()));
            assert!(document.update(cx, |doc, cx| doc.save(cx)).await);
            let saved = load_off_ui(&path, cx).await;
            assert_eq!(saved.text, recovered);
            document.read_with(cx, |doc, _| {
                assert!(!doc.is_dirty());
                assert!(doc.notice().is_none());
                assert!(doc.protected_save_source().is_none());
                assert_eq!(doc.stamp, saved.stamp);
            });
            document.update(cx, |doc, cx| {
                doc.apply_edits(vec![(0..doc.text.len(), "next".into())], None, cx);
            });
            assert!(document.update(cx, |doc, cx| doc.save(cx)).await);
            assert_eq!(read_off_ui(&path, cx).await, b"next");
            assert!(!document.read_with(cx, |doc, _| doc.is_dirty()));
        }
    }

    #[gpui::test]
    async fn legacy_local_recovery_requires_another_path_even_if_the_source_is_missing(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        for exists in [true, false] {
            let (path, renamed, destination, loaded) = cx
                .background_spawn(async move {
                    let directory = db::testing::dir("document-local-recovery-legacy");
                    let path = directory.join("file.txt");
                    let loaded = if exists {
                        std::fs::write(&path, "today").unwrap();
                        load(&path).unwrap()
                    } else {
                        Loaded::of(b"")
                    };
                    (
                        path,
                        directory.join("renamed.txt"),
                        directory.join("new.txt"),
                        loaded,
                    )
                })
                .await;
            let recovered = if exists { "today" } else { "unsaved" };
            let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
            assert!(document.update(cx, |doc, cx| doc.recover(recovered, None, cx)));
            document.read_with(cx, |doc, _| {
                assert!(doc.is_dirty());
                assert!(doc.stamp.is_none());
                assert!(doc.recovery_snapshot().2.is_none());
                assert_eq!(doc.protected_save_source(), Some(path.clone()));
            });
            assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
            document.update(cx, |doc, cx| {
                doc.set_save_target(path.clone(), loaded.stamp.clone(), true, true, cx);
            });
            assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
            let from = path.clone();
            let to = renamed.clone();
            cx.background_spawn(async move {
                if exists {
                    assert_eq!(std::fs::read(&from).unwrap(), b"today");
                    std::fs::rename(from, to).unwrap();
                } else {
                    assert!(!from.exists());
                }
            })
            .await;
            document.update(cx, |doc, cx| doc.renamed(renamed.clone(), cx));
            assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
            assert_eq!(
                document.read_with(cx, |doc, _| doc.protected_save_source()),
                Some(renamed.clone())
            );
            if exists {
                assert_eq!(read_off_ui(&renamed, cx).await, b"today");
            } else {
                cx.background_spawn(async move { assert!(!renamed.exists()) })
                    .await;
            }
            document.update(cx, |doc, cx| {
                doc.set_save_target(destination.clone(), None, false, false, cx);
            });
            assert!(document.update(cx, |doc, cx| doc.save(cx)).await);
            assert_eq!(read_off_ui(&destination, cx).await, recovered.as_bytes());
            assert!(!document.read_with(cx, |doc, _| doc.is_dirty()));
            assert!(document.read_with(cx, |doc, _| doc.protected_save_source().is_none()));
        }
    }

    #[gpui::test]
    async fn missing_local_recovery_source_does_not_grant_creation_permission(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let (path, destination, original, chosen) = cx
            .background_spawn(async move {
                let directory = db::testing::dir("document-local-recovery-missing");
                let path = directory.join("file.txt");
                let destination = directory.join("chosen.txt");
                std::fs::write(&path, "before").unwrap();
                let original = load(&path).unwrap().stamp;
                std::fs::remove_file(&path).unwrap();
                std::fs::write(&destination, "other").unwrap();
                let chosen = crate::remote::revision(&destination).unwrap();
                (path, destination, original, chosen)
            })
            .await;
        let document = cx.new(|cx| Document::new(Some(path.clone()), "", cx));
        document.update(cx, |doc, cx| doc.recover("unsaved", original.clone(), cx));
        assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
        document.read_with(cx, |doc, _| {
            assert!(doc.is_dirty());
            assert_eq!(doc.recovery_snapshot().2, original);
        });
        cx.background_spawn(async move { assert!(!path.exists()) })
            .await;
        document.update(cx, |doc, cx| {
            doc.set_save_target(destination.clone(), chosen, false, false, cx);
        });
        assert!(document.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(read_off_ui(&destination, cx).await, b"unsaved");
        assert!(!document.read_with(cx, |doc, _| doc.is_dirty()));
    }

    #[gpui::test]
    async fn failed_local_recovery_save_as_keeps_the_original_snapshot_after_undo(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let (path, blocked, destination, original, loaded) = cx
            .background_spawn(async move {
                let directory = db::testing::dir("document-local-recovery-save-as-undo");
                let path = directory.join("file.txt");
                std::fs::write(&path, "before").unwrap();
                let original = load(&path).unwrap().stamp;
                std::fs::write(&path, "today").unwrap();
                let loaded = load(&path).unwrap();
                let blocked = directory.join("blocked");
                std::fs::write(&blocked, "not a directory").unwrap();
                (
                    path,
                    blocked.join("file.txt"),
                    directory.join("new.txt"),
                    original,
                    loaded,
                )
            })
            .await;
        let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        let origin = document.entity_id();
        document.update(cx, |doc, cx| {
            doc.recover("unsaved", original.clone(), cx);
            doc.set_save_target(blocked, None, false, false, cx);
        });
        assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
        document.update(cx, |doc, cx| {
            doc.undo(origin, cx).unwrap();
            assert_eq!(doc.text.rope().to_string(), "today");
            assert!(doc.is_dirty());
            assert_eq!(doc.protected_save_source(), Some(path.clone()));
            let snapshot = doc.recovery_snapshot();
            assert_eq!(snapshot.0, Some(path.clone()));
            assert_eq!(snapshot.1.to_string(), "unsaved");
            assert_eq!(snapshot.2, original);
        });
        assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(
            document.read_with(cx, |doc, _| doc.recovery_snapshot().1.to_string()),
            "unsaved"
        );
        document.update(cx, |doc, cx| {
            doc.set_save_target(path.clone(), loaded.stamp.clone(), false, true, cx);
        });
        assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(read_off_ui(&path, cx).await, b"today");
        document.update(cx, |doc, cx| {
            doc.redo(origin, cx).unwrap();
            doc.set_save_target(destination.clone(), None, false, false, cx);
        });
        assert!(document.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(read_off_ui(&destination, cx).await, b"unsaved");
        assert!(!document.read_with(cx, |doc, _| doc.is_dirty()));
        assert!(document.read_with(cx, |doc, _| doc.protected_save_source().is_none()));
    }

    #[gpui::test]
    async fn untitled_recovery_can_still_be_saved_to_its_first_path(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let document = cx.new(|cx| Document::new(None, "", cx));
        assert!(document.update(cx, |doc, cx| doc.recover("unsaved", None, cx)));
        document.read_with(cx, |doc, _| {
            assert!(doc.is_dirty());
            assert!(doc.protected_save_source().is_none());
            assert!(doc.recovery_snapshot().0.is_none());
        });
        let destination = cx
            .background_spawn(async move {
                db::testing::dir("document-untitled-recovery").join("new.txt")
            })
            .await;
        document.update(cx, |doc, cx| {
            doc.set_save_target(destination.clone(), None, false, false, cx);
        });
        assert!(document.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(read_off_ui(&destination, cx).await, b"unsaved");
        assert!(!document.read_with(cx, |doc, _| doc.is_dirty()));
    }

    #[gpui::test]
    async fn save_as_uses_the_selected_destination_revision(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let dir = db::testing::dir("document-local-save-as");
        let path = dir.join("source.txt");
        let destination = dir.join("dest.txt");
        std::fs::write(&path, "source").unwrap();
        std::fs::write(&destination, "before").unwrap();
        let loaded = load(&path).unwrap();
        let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        let chosen = crate::remote::revision(&destination).unwrap();
        document.update(cx, |d, cx| {
            d.set_save_target(destination.clone(), chosen, false, false, cx)
        });
        std::fs::write(&destination, "external").unwrap();
        assert!(!document.update(cx, |d, cx| d.save(cx)).await);
        assert_eq!(std::fs::read(&destination).unwrap(), b"external");
        assert!(document.read_with(cx, |d, _| d.is_dirty()));
        let chosen = crate::remote::revision(&destination).unwrap();
        let second = cx.new(|cx| Document::new(None, "replacement", cx));
        second.update(cx, |d, cx| {
            d.set_save_target(destination.clone(), chosen, false, false, cx)
        });
        assert!(second.update(cx, |d, cx| d.save(cx)).await);
        assert_eq!(std::fs::read(&destination).unwrap(), b"replacement");
        assert_eq!(std::fs::read(&path).unwrap(), b"source");
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn save_as_source_aliases_do_not_authorize_new_external_revisions(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::os::unix::fs::symlink;
        cx.executor().allow_parking();
        let (dir, path, alias, loaded) = cx
            .background_spawn(async move {
                let dir = db::testing::dir("document-source-alias");
                let path = dir.join("source.txt");
                let alias = dir.join("alias.txt");
                std::fs::write(&path, "before").unwrap();
                symlink(&path, &alias).unwrap();
                let loaded = load(&path).unwrap();
                (dir, path, alias, loaded)
            })
            .await;
        let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        document.update(cx, |d, cx| {
            d.apply_edits(vec![(0..6, "ours".into())], None, cx)
        });
        let source = path.clone();
        let target = alias.clone();
        let (now, same_recovery) = cx
            .background_spawn(async move {
                std::fs::write(&source, "external").unwrap();
                (
                    crate::remote::revision(&target).unwrap(),
                    crate::remote::same_file(&source, &target).unwrap(),
                )
            })
            .await;
        assert!(same_recovery);
        document.update(cx, |d, cx| {
            d.set_save_target(alias.clone(), now.clone(), true, false, cx)
        });
        assert!(!document.update(cx, |d, cx| d.save(cx)).await);
        assert_eq!(read_off_ui(&path, cx).await, b"external");
        assert_eq!(document.read_with(cx, |d, _| d.stamp.clone()), loaded.stamp);
        let restarted = load_off_ui(&path, cx).await;
        for original in [loaded.stamp.clone(), None] {
            let recovered = cx.new(|cx| Document::open(path.clone(), &restarted, cx));
            recovered.update(cx, |d, cx| {
                d.recover("unsaved", original.clone(), cx);
                d.set_path(dir.join("attempt.txt"), cx);
                assert_eq!(d.protected_save_source(), Some(path.clone()));
                d.set_save_target(alias.clone(), now.clone(), false, same_recovery, cx);
            });
            assert!(!recovered.update(cx, |d, cx| d.save(cx)).await);
            assert_eq!(read_off_ui(&path, cx).await, b"external");
            recovered.read_with(cx, |d, _| {
                assert!(d.is_dirty());
                assert_eq!(d.stamp, original);
                assert_eq!(d.protected_save_source(), Some(alias.clone()));
                let snapshot = d.recovery_snapshot();
                assert_eq!(snapshot.0, Some(alias.clone()));
                assert_eq!(snapshot.1.to_string(), "unsaved");
                assert_eq!(snapshot.2, original);
            });
        }
    }

    #[gpui::test]
    async fn weak_local_recovery_stamp_requires_a_different_destination(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let path = db::testing::dir("local-recovery-weak-stamp").join("file.txt");
        std::fs::write(&path, "disk").unwrap();
        let loaded = load(&path).unwrap();
        let mut stamp = loaded.stamp.clone().unwrap();
        stamp.hash = None;
        let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        document.update(cx, |doc, cx| doc.recover("recovered", Some(stamp), cx));
        assert!(document.read_with(cx, |doc, _| doc.recovery_snapshot().2.is_none()));
        assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "disk");
        let rescued = path.with_file_name("copy.txt");
        document.update(cx, |doc, cx| doc.set_path(rescued.clone(), cx));
        assert!(document.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(std::fs::read_to_string(rescued).unwrap(), "recovered");
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn legacy_remote_recovery_requires_save_as_even_when_contents_match(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let directory = db::testing::dir("document-recovery-legacy")
            .canonicalize()
            .unwrap();
        let source = directory.join("server");
        let cache = directory.join("cache");
        std::fs::create_dir(&source).unwrap();
        let target = source.join("file.txt");
        let path = cache.join("file.txt");
        std::fs::write(&target, b"today").unwrap();
        let _mount = crate::remote::fixture(&source, &cache);
        let loaded = load(&path).unwrap();
        let restored = cx.new(|cx| Document::open(path, &loaded, cx));
        restored.update(cx, |doc, cx| doc.recover("today", None, cx));
        assert!(restored.read_with(cx, |doc, _| doc.is_dirty()));
        assert_eq!(
            restored.read_with(cx, |doc, _| doc.recovery_snapshot().2),
            None
        );
        assert!(!restored.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(std::fs::read(&target).unwrap(), b"today");

        // A rename retains the unknown revision rather than authorizing
        // an overwrite of another existing target.
        let renamed = cache.join("renamed.txt");
        std::fs::write(source.join("renamed.txt"), b"other work").unwrap();
        restored.update(cx, |doc, cx| doc.renamed(renamed, cx));
        assert!(!restored.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(
            std::fs::read(source.join("renamed.txt")).unwrap(),
            b"other work"
        );

        restored.update(cx, |doc, cx| doc.set_path(cache.join("new.txt"), cx));
        assert!(restored.read_with(cx, |doc, _| doc.is_dirty()));
        assert!(restored.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(std::fs::read(source.join("new.txt")).unwrap(), b"today");
        assert!(!restored.read_with(cx, |doc, _| doc.is_dirty()));
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn unchanged_remote_recovery_can_be_saved_and_cleared(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let directory = db::testing::dir("document-recovery-save")
            .canonicalize()
            .unwrap();
        let source = directory.join("server");
        let cache = directory.join("cache");
        std::fs::create_dir(&source).unwrap();
        let target = source.join("file.txt");
        let path = cache.join("file.txt");
        std::fs::write(&target, b"before").unwrap();
        let _mount = crate::remote::fixture(&source, &cache);
        let loaded = load(&path).unwrap();
        let restored = cx.new(|cx| Document::open(path, &loaded, cx));
        restored.update(cx, |doc, cx| {
            doc.recover("unsaved", loaded.stamp.clone(), cx)
        });
        restored.update(cx, |doc, cx| {
            doc.set_path(doc.path().unwrap().to_path_buf(), cx)
        });
        assert_eq!(
            restored.read_with(cx, |doc, _| doc.stamp.clone()),
            loaded.stamp
        );
        assert!(restored.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(std::fs::read(&target).unwrap(), b"unsaved");
        assert!(!restored.read_with(cx, |doc, _| doc.is_dirty()));
        assert!(restored.read_with(cx, |doc, _| doc.recovery_source.is_none()));
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn queued_remote_saves_use_completed_revisions_and_expose_the_current_tail(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let directory = db::testing::dir("document-save-queue")
            .canonicalize()
            .unwrap();
        let source = directory.join("server");
        let cache = directory.join("cache");
        std::fs::create_dir(&source).unwrap();
        let target = source.join("file.txt");
        let path = cache.join("file.txt");
        std::fs::write(&target, b"before").unwrap();
        let _mount = crate::remote::fixture(&source, &cache);
        let loaded = load(&path).unwrap();
        let document = cx.new(|cx| Document::open(path, &loaded, cx));
        let (release_first, first, first_tail) = document.update(cx, |doc, cx| {
            doc.recover("first", loaded.stamp.clone(), cx);
            let release = hold_save_queue(doc, cx);
            let save = doc.save(cx);
            assert_eq!(doc.pending_saves, 1);
            (release, save, doc.pending_save().unwrap())
        });
        let (release_second, second) = document.update(cx, |doc, cx| {
            // Keep the second write stopped after the first completes, so
            // inspecting the first result cannot race with its replacement.
            let release = hold_save_queue(doc, cx);
            doc.apply_edits(vec![(0..doc.text().len(), "second".into())], None, cx);
            let save = doc.save(cx);
            assert_eq!(doc.pending_saves, 2);
            assert!(doc.is_saving());
            assert!(doc.is_dirty());
            (release, save)
        });
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let changes = changes.clone();
            cx.subscribe(&document, move |document, event, cx| {
                if matches!(event, DocumentEvent::DirtyChanged) {
                    changes.borrow_mut().push(document.read(cx).pending_saves);
                }
            })
        });

        release_first.send(()).unwrap();
        assert!(first.await);
        assert!(first_tail.await);
        assert_eq!(std::fs::read(&target).unwrap(), b"first");
        assert!(document.read_with(cx, |doc, _| doc.recovery_source.is_none()));
        assert_eq!(document.read_with(cx, |doc, _| doc.pending_saves), 1);
        // A waiter on the old tail must recapture after awaiting it.
        let tail = document.read_with(cx, |doc, _| doc.pending_save().unwrap());
        release_second.send(()).unwrap();
        assert!(tail.await);
        assert!(second.await);
        assert_eq!(std::fs::read(&target).unwrap(), b"second");
        assert!(document.read_with(cx, |doc, _| doc.pending_save().is_none()));
        assert!(!document.read_with(cx, |doc, _| doc.is_dirty() || doc.is_saving()));
        assert_eq!(*changes.borrow(), vec![1, 0]);
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn failed_queued_save_leaves_the_revision_for_the_next_attempt(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let directory = db::testing::dir("document-save-queue-failure")
            .canonicalize()
            .unwrap();
        let source = directory.join("server");
        let cache = directory.join("cache");
        std::fs::create_dir(&source).unwrap();
        let target = source.join("file.txt");
        let path = cache.join("file.txt");
        std::fs::write(&target, b"before").unwrap();
        let _mount = crate::remote::fixture(&source, &cache);
        let loaded = load(&path).unwrap();
        let document = cx.new(|cx| Document::open(path, &loaded, cx));
        let (first, release, second) = document.update(cx, |doc, cx| {
            doc.recover("😀", loaded.stamp.clone(), cx);
            doc.set_encoding(text::encoding::Encoding::Windows1252, cx);
            let first = doc.save(cx);
            let release = hold_save_queue(doc, cx);
            doc.apply_edits(vec![(0..doc.text().len(), "retry".into())], None, cx);
            doc.set_encoding(text::encoding::Encoding::Utf8, cx);
            (first, release, doc.save(cx))
        });
        assert!(!first.await);
        assert_eq!(std::fs::read(&target).unwrap(), b"before");
        document.read_with(cx, |doc, _| {
            assert_eq!(doc.stamp, loaded.stamp);
            assert!(doc.recovery_source.is_some());
            assert!(doc.notice().is_some());
            assert_eq!(doc.pending_saves, 1);
            assert!(doc.is_dirty());
        });
        release.send(()).unwrap();
        assert!(second.await);
        assert_eq!(std::fs::read(&target).unwrap(), b"retry");
        assert!(document.read_with(cx, |doc, _| doc.recovery_source.is_none()));
        assert!(!document.read_with(cx, |doc, _| doc.is_dirty() || doc.is_saving()));
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn queued_save_rejects_a_changed_path_without_clearing_recovery(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let directory = db::testing::dir("document-save-queue-path")
            .canonicalize()
            .unwrap();
        let source = directory.join("server");
        let cache = directory.join("cache");
        std::fs::create_dir(&source).unwrap();
        let target = source.join("file.txt");
        let path = cache.join("file.txt");
        std::fs::write(&target, b"before").unwrap();
        let _mount = crate::remote::fixture(&source, &cache);
        let loaded = load(&path).unwrap();
        let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        let (release_first, first, release_second, second) = document.update(cx, |doc, cx| {
            doc.recover("unsaved", loaded.stamp.clone(), cx);
            let release_first = hold_save_queue(doc, cx);
            let first = doc.save(cx);
            let release_second = hold_save_queue(doc, cx);
            doc.set_path(cache.join("new.txt"), cx);
            (release_first, first, release_second, doc.save(cx))
        });
        release_first.send(()).unwrap();
        assert!(!first.await);
        assert_eq!(std::fs::read(&target).unwrap(), b"before");
        assert!(!source.join("new.txt").exists());
        document.read_with(cx, |doc, _| {
            assert_eq!(doc.pending_saves, 1);
            assert!(doc.text.is_dirty());
            assert_eq!(doc.recovery_snapshot().0, Some(path));
            assert!(doc.recovery_source.is_some());
        });
        release_second.send(()).unwrap();
        assert!(second.await);
        assert_eq!(std::fs::read(source.join("new.txt")).unwrap(), b"unsaved");
        assert!(document.read_with(cx, |doc, _| doc.recovery_source.is_none()));
        assert!(!document.read_with(cx, |doc, _| doc.is_dirty() || doc.is_saving()));
    }

    #[gpui::test]
    fn save_as_invalidates_the_clean_checkpoint_but_physical_rename_does_not(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = db::testing::dir("document-save-as-checkpoint")
            .canonicalize()
            .unwrap();
        let path = directory.join("file.txt");
        let renamed = directory.join("renamed.txt");
        std::fs::write(&path, b"original").unwrap();
        let loaded = load(&path).unwrap();
        let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        let changes = std::rc::Rc::new(std::cell::Cell::new(0));
        let _subscription = cx.update(|cx| {
            let changes = changes.clone();
            cx.subscribe(&document, move |_, event, _| {
                if matches!(event, DocumentEvent::DirtyChanged) {
                    changes.set(changes.get() + 1);
                }
            })
        });

        std::fs::rename(&path, &renamed).unwrap();
        document.update(cx, |doc, cx| {
            let version = doc.version();
            doc.renamed(renamed.clone(), cx);
            assert_eq!(doc.stamp, loaded.stamp);
            assert_eq!(doc.version(), version);
            assert!(!doc.is_dirty());
            doc.set_path(renamed, cx);
            assert_eq!(doc.version(), version);
            assert!(!doc.is_dirty());
        });
        assert_eq!(changes.get(), 0);
        document.update(cx, |doc, cx| {
            doc.set_path(directory.join("new.txt"), cx);
            assert!(doc.text.is_dirty());
            assert!(doc.is_dirty());
        });
        assert_eq!(changes.get(), 1);
    }

    #[gpui::test]
    async fn failed_save_as_after_old_path_write_keeps_undone_text_dirty(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let directory = db::testing::dir("document-save-as-pending-undo")
            .canonicalize()
            .unwrap();
        let path = directory.join("file.txt");
        let blocked = directory.join("blocked");
        std::fs::write(&path, b"original").unwrap();
        // A regular file as the parent fails on every platform, regardless
        // of the test runner's permission to write protected directories.
        std::fs::write(&blocked, b"not a directory").unwrap();
        let destination = blocked.join("file.txt");
        let loaded = load(&path).unwrap();
        let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        let origin = document.entity_id();
        let (release_first, first) = document.update(cx, |doc, cx| {
            let release = hold_save_queue(doc, cx);
            doc.apply_edits(vec![(0..doc.text().len(), "edited".into())], None, cx);
            let save = doc.save(cx);
            doc.undo(origin, cx).unwrap();
            assert_eq!(doc.text.rope().to_string(), "original");
            assert!(!doc.text.is_dirty());
            assert!(doc.is_saving());
            (release, save)
        });
        release_first.send(()).unwrap();
        // Drain background work after each foreground step. Once the real
        // write finishes, its foreground completion stays queued until the
        // path has changed, so no timing or filesystem speed decides the race.
        while std::fs::read(&path).unwrap() != b"edited" {
            assert!(cx.dispatcher.tick(false), "save did not reach the write");
            while cx.dispatcher.tick(true) {}
        }
        let (release_second, second) = document.update(cx, |doc, cx| {
            assert_eq!(doc.pending_saves, 1);
            assert!(!doc.text.is_dirty());
            let release = hold_save_queue(doc, cx);
            doc.set_path(destination.clone(), cx);
            assert!(doc.text.is_dirty());
            (release, doc.save(cx))
        });
        assert!(!first.await);
        assert_eq!(std::fs::read(&path).unwrap(), b"edited");
        document.read_with(cx, |doc, _| {
            assert_eq!(doc.pending_saves, 1);
            assert_eq!(doc.text.rope().to_string(), "original");
            assert!(doc.text.is_dirty());
            assert!(doc.is_dirty());
        });
        release_second.send(()).unwrap();
        assert!(!second.await);
        assert!(!destination.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"edited");
        document.read_with(cx, |doc, _| {
            assert_eq!(doc.path(), Some(destination.as_path()));
            assert_eq!(doc.text.rope().to_string(), "original");
            assert!(!doc.is_saving());
            assert!(doc.pending_save().is_none());
            assert!(doc.notice().is_some());
            assert!(doc.text.is_dirty());
            assert!(doc.is_dirty());
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn async_save_marks_the_written_checkpoint_after_edit_and_undo(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let directory = db::testing::dir("document-save-checkpoint")
            .canonicalize()
            .unwrap();
        let source = directory.join("server");
        let cache = directory.join("cache");
        std::fs::create_dir(&source).unwrap();
        let target = source.join("file.txt");
        let path = cache.join("file.txt");
        std::fs::write(&target, b"before").unwrap();
        let _mount = crate::remote::fixture(&source, &cache);
        let loaded = load(&path).unwrap();
        let document = cx.new(|cx| Document::open(path, &loaded, cx));
        document.update(cx, |doc, cx| {
            doc.recover("unsaved", loaded.stamp.clone(), cx)
        });
        let origin = document.entity_id();

        // Do the edits in this update, before the spawned IO can run.
        // Undo returns to the saved snapshot with a different version.
        let save = document.update(cx, |doc, cx| {
            let save = doc.save(cx);
            doc.apply_edits(vec![(0..doc.text().len(), "later".into())], None, cx);
            doc.undo(origin, cx).unwrap();
            assert_eq!(doc.text().rope().to_string(), "unsaved");
            assert!(doc.is_dirty());
            assert!(doc.is_saving());
            assert!(doc.pending_save().is_some());
            assert!(doc.recovery_source.is_some());
            save
        });
        assert!(save.await);
        assert_eq!(std::fs::read(&target).unwrap(), b"unsaved");
        assert!(!document.read_with(cx, |doc, _| doc.is_dirty()));
        assert!(document.read_with(cx, |doc, _| doc.pending_save().is_none()));
        assert!(document.read_with(cx, |doc, _| doc.recovery_source.is_none()));

        // Undo to the old clean text while saving new text. That text
        // becomes dirty when the target has actually received the new one.
        let save = document.update(cx, |doc, cx| {
            doc.apply_edits(vec![(0..doc.text().len(), "next".into())], None, cx);
            let save = doc.save(cx);
            doc.undo(origin, cx).unwrap();
            assert_eq!(doc.text().rope().to_string(), "unsaved");
            assert!(!doc.text.is_dirty());
            assert!(doc.is_dirty());
            assert!(doc.is_saving());
            assert!(doc.pending_save().is_some());
            save
        });
        assert!(save.await);
        assert_eq!(std::fs::read(&target).unwrap(), b"next");
        assert!(document.read_with(cx, |doc, _| doc.is_dirty()));
        assert!(!document.read_with(cx, |doc, _| doc.is_saving()));
        assert_eq!(
            document.read_with(cx, |doc, _| doc.recovery_snapshot().1.to_string()),
            "unsaved"
        );
        document.update(cx, |doc, cx| doc.redo(origin, cx)).unwrap();
        assert_eq!(
            document.read_with(cx, |doc, _| doc.text().rope().to_string()),
            "next"
        );
        assert!(!document.read_with(cx, |doc, _| doc.is_dirty()));
    }

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
