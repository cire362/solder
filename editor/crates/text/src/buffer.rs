use std::{borrow::Cow, ops::Range, sync::Arc, time::Instant};

use ropey::{Rope, RopeSlice};

use crate::{History, Selection};

/// A row/column position. `column` is a byte offset within the row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub row: usize,
    pub column: usize,
}

impl Point {
    pub const fn new(row: usize, column: usize) -> Self {
        Self { row, column }
    }
}

/// One applied change, in the shape tree-sitter's `InputEdit` expects.
///
/// Edits are reported in the order they were applied, so each one is valid
/// against the text as it was right before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
    pub start_point: Point,
    pub old_end_point: Point,
    pub new_end_point: Point,
    /// `start_point` and `old_end_point` with columns in UTF-16 code units,
    /// which is what most language servers expect.
    pub start_utf16: Point,
    pub old_end_utf16: Point,
    pub new_text: Arc<str>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

pub struct Buffer {
    rope: Rope,
    line_ending: LineEnding,
    version: u64,
    saved_version: u64,
    history: History,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new("")
    }
}

impl Buffer {
    /// Builds a buffer, normalizing `\r\n` to `\n`. The original ending is kept
    /// for saving, so a CRLF file round-trips unchanged.
    pub fn new(text: &str) -> Self {
        let line_ending = if text.contains("\r\n") {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        };
        let text: Cow<str> = match line_ending {
            LineEnding::Lf => Cow::Borrowed(text),
            LineEnding::CrLf => Cow::Owned(text.replace("\r\n", "\n")),
        };
        Self {
            rope: Rope::from_str(&text),
            line_ending,
            version: 0,
            saved_version: 0,
            history: History::default(),
        }
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }

    pub fn mark_saved(&mut self) {
        self.saved_version = self.version;
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn len(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    /// Number of rows. An empty buffer and a buffer ending in `\n` both have a
    /// final empty row, matching how the cursor can sit there.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Row contents without the trailing newline.
    pub fn line(&self, row: usize) -> RopeSlice<'_> {
        let line = self.rope.line(row);
        let len = line.len_chars();
        if len > 0 && line.char(len - 1) == '\n' {
            line.slice(..len - 1)
        } else {
            line
        }
    }

    /// Row contents as a string, borrowed when the row sits in one rope chunk.
    pub fn line_str(&self, row: usize) -> Cow<'_, str> {
        let line = self.line(row);
        match line.as_str() {
            Some(s) => Cow::Borrowed(s),
            None => Cow::Owned(line.to_string()),
        }
    }

    pub fn line_len(&self, row: usize) -> usize {
        self.line(row).len_bytes()
    }

    pub fn line_start(&self, row: usize) -> usize {
        self.rope.line_to_byte(row)
    }

    pub fn max_point(&self) -> Point {
        let row = self.line_count() - 1;
        Point::new(row, self.line_len(row))
    }

    pub fn offset_to_point(&self, offset: usize) -> Point {
        let offset = self.clip_offset(offset);
        let row = self.rope.byte_to_line(offset);
        Point::new(row, offset - self.rope.line_to_byte(row))
    }

    /// Clamps the point into the buffer and onto a char boundary.
    pub fn point_to_offset(&self, point: Point) -> usize {
        let max = self.max_point();
        if point.row > max.row {
            return self.len();
        }
        let column = point.column.min(self.line_len(point.row));
        self.clip_offset(self.line_start(point.row) + column)
    }

    /// Rounds down to the nearest char boundary.
    pub fn clip_offset(&self, offset: usize) -> usize {
        let offset = offset.min(self.len());
        let char_idx = self.rope.byte_to_char(offset);
        self.rope.char_to_byte(char_idx)
    }

    pub fn text_for_range(&self, range: Range<usize>) -> String {
        self.slice(range).to_string()
    }

    pub fn slice(&self, range: Range<usize>) -> RopeSlice<'_> {
        let start = self.rope.byte_to_char(self.clip_offset(range.start));
        let end = self.rope.byte_to_char(self.clip_offset(range.end));
        self.rope.slice(start..end)
    }

    pub fn char_at(&self, offset: usize) -> Option<char> {
        (offset < self.len()).then(|| self.rope.char(self.rope.byte_to_char(offset)))
    }

    pub fn char_before(&self, offset: usize) -> Option<char> {
        let idx = self.rope.byte_to_char(self.clip_offset(offset));
        (idx > 0).then(|| self.rope.char(idx - 1))
    }

    /// Full text with the file's original line endings, ready to write to disk.
    pub fn text_for_save(&self) -> String {
        match self.line_ending {
            LineEnding::Lf => self.rope.to_string(),
            LineEnding::CrLf => self.rope.to_string().replace('\n', "\r\n"),
        }
    }

    /// Applies non-overlapping edits as one undo step and returns them in the
    /// order they were applied (highest offset first).
    ///
    /// Consecutive calls within [`History::GROUP_INTERVAL`] merge into a single
    /// undo step unless [`Buffer::seal_history`] was called in between.
    pub fn edit<S: AsRef<str>>(
        &mut self,
        edits: impl IntoIterator<Item = (Range<usize>, S)>,
        selections_before: &[Selection],
        now: Instant,
    ) -> Vec<Edit> {
        let mut edits: Vec<(Range<usize>, S)> = edits
            .into_iter()
            .map(|(r, s)| (self.clip_offset(r.start)..self.clip_offset(r.end), s))
            .filter(|(r, s)| !r.is_empty() || !s.as_ref().is_empty())
            .collect();
        if edits.is_empty() {
            return Vec::new();
        }
        edits.sort_by(|a, b| b.0.start.cmp(&a.0.start));

        let mut applied = Vec::with_capacity(edits.len());
        let mut record = Vec::with_capacity(edits.len());
        for (range, text) in edits {
            let text = text.as_ref();
            let old_text = self.text_for_range(range.clone());
            let edit = self.apply(range.clone(), text);
            record.push(crate::history::Change {
                start: range.start,
                old_text,
                new_text: text.to_owned(),
            });
            applied.push(edit);
        }
        self.history.push(record, selections_before, now);
        applied
    }

    /// Records the selections to restore when this step is redone.
    pub fn set_selections_after(&mut self, selections: &[Selection]) {
        self.history.set_selections_after(selections);
    }

    /// Stops the next edit from merging into the current undo step.
    pub fn seal_history(&mut self) {
        self.history.seal();
    }

    pub fn undo(&mut self) -> Option<(Vec<Edit>, Vec<Selection>)> {
        let tx = self.history.pop_undo()?;
        let mut edits = Vec::new();
        // Revert in reverse application order: the last change applied is the
        // only one whose recorded offset is valid against the current text.
        for change in tx.changes.iter().rev() {
            let range = change.start..change.start + change.new_text.len();
            edits.push(self.apply(range, &change.old_text));
        }
        let selections = tx.before.clone();
        self.history.push_redo(tx);
        Some((edits, selections))
    }

    pub fn redo(&mut self) -> Option<(Vec<Edit>, Vec<Selection>)> {
        let tx = self.history.pop_redo()?;
        let mut edits = Vec::new();
        // Replay in the original application order, which keeps every
        // recorded offset valid.
        for change in tx.changes.iter() {
            let range = change.start..change.start + change.old_text.len();
            edits.push(self.apply(range, &change.new_text));
        }
        let selections = tx.after.clone();
        self.history.push_undo_after_redo(tx);
        Some((edits, selections))
    }

    fn apply(&mut self, range: Range<usize>, text: &str) -> Edit {
        let start_point = self.offset_to_point(range.start);
        let old_end_point = self.offset_to_point(range.end);
        let start_utf16 = self.point_to_utf16(start_point);
        let old_end_utf16 = self.point_to_utf16(old_end_point);
        let start_char = self.rope.byte_to_char(range.start);
        let end_char = self.rope.byte_to_char(range.end);
        if end_char > start_char {
            self.rope.remove(start_char..end_char);
        }
        if !text.is_empty() {
            self.rope.insert(start_char, text);
        }
        self.version += 1;
        let new_end = range.start + text.len();
        Edit {
            start: range.start,
            old_end: range.end,
            new_end,
            start_point,
            old_end_point,
            new_end_point: self.offset_to_point(new_end),
            start_utf16,
            old_end_utf16,
            new_text: text.into(),
        }
    }

    /// The same position with the column counted in UTF-16 code units.
    pub fn point_to_utf16(&self, point: Point) -> Point {
        let offset = self.point_to_offset(point);
        let line_start = self.line_start(point.row);
        let rope = &self.rope;
        let col = rope.char_to_utf16_cu(rope.byte_to_char(offset))
            - rope.char_to_utf16_cu(rope.byte_to_char(line_start));
        Point::new(point.row, col)
    }

    pub fn offset_to_utf16(&self, offset: usize) -> Point {
        self.point_to_utf16(self.offset_to_point(offset))
    }

    /// Byte offset of a row and UTF-16 column, clamped into the buffer.
    pub fn utf16_to_offset(&self, point: Point) -> usize {
        if point.row >= self.line_count() {
            return self.len();
        }
        let rope = &self.rope;
        let line_start = self.line_start(point.row);
        let base = rope.char_to_utf16_cu(rope.byte_to_char(line_start));
        let line_end = line_start + self.line_len(point.row);
        let end = rope.char_to_utf16_cu(rope.byte_to_char(line_end));
        let target = (base + point.column).min(end);
        rope.char_to_byte(rope.utf16_cu_to_char(target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn lines_exclude_newline() {
        let b = Buffer::new("ab\ncd\n");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line(0).to_string(), "ab");
        assert_eq!(b.line(1).to_string(), "cd");
        assert_eq!(b.line(2).to_string(), "");
        assert_eq!(b.max_point(), Point::new(2, 0));
    }

    #[test]
    fn crlf_round_trips() {
        let b = Buffer::new("a\r\nb\r\n");
        assert_eq!(b.rope().to_string(), "a\nb\n");
        assert_eq!(b.text_for_save(), "a\r\nb\r\n");
    }

    #[test]
    fn points_and_offsets_handle_multibyte() {
        let b = Buffer::new("héllo\nмир");
        assert_eq!(b.offset_to_point(7), Point::new(1, 0));
        assert_eq!(b.point_to_offset(Point::new(1, 2)), 9);
        // Column 3 is inside "и", so it clips back to the char start.
        assert_eq!(b.point_to_offset(Point::new(1, 3)), 9);
        assert_eq!(b.point_to_offset(Point::new(9, 0)), b.len());
    }

    #[test]
    fn edit_reports_tree_sitter_coordinates() {
        let mut b = Buffer::new("fn main() {}\n");
        let edits = b.edit([(11..11, "\n    x\n")], &[], now());
        assert_eq!(b.rope().to_string(), "fn main() {\n    x\n}\n");
        assert_eq!(
            edits,
            vec![Edit {
                start: 11,
                old_end: 11,
                new_end: 18,
                start_point: Point::new(0, 11),
                old_end_point: Point::new(0, 11),
                new_end_point: Point::new(2, 0),
                start_utf16: Point::new(0, 11),
                old_end_utf16: Point::new(0, 11),
                new_text: "\n    x\n".into(),
            }]
        );
    }

    #[test]
    fn utf16_columns_count_surrogate_pairs() {
        // "é" is one UTF-16 unit, the emoji is two, and both are multi-byte.
        let b = Buffer::new("aé😀b\nx");
        assert_eq!(b.offset_to_utf16(1 + 2 + 4), Point::new(0, 4));
        assert_eq!(b.utf16_to_offset(Point::new(0, 4)), 7);
        assert_eq!(b.utf16_to_offset(Point::new(0, 99)), 8);
        assert_eq!(b.utf16_to_offset(Point::new(1, 1)), 10);
        assert_eq!(b.utf16_to_offset(Point::new(5, 0)), b.len());
    }

    #[test]
    fn multi_edit_applies_back_to_front() {
        let mut b = Buffer::new("a b c");
        let edits = b.edit([(0..1, "xx"), (4..5, "zz"), (2..3, "yy")], &[], now());
        assert_eq!(b.rope().to_string(), "xx yy zz");
        assert_eq!(edits.iter().map(|e| e.start).collect::<Vec<_>>(), [4, 2, 0]);
    }

    #[test]
    fn undo_redo_restore_text_and_selections() {
        let t = now();
        let mut b = Buffer::new("hello");
        let before = [Selection::cursor(5)];
        b.edit([(5..5, " world")], &before, t);
        b.set_selections_after(&[Selection::cursor(11)]);
        b.seal_history();
        b.edit([(0..5, "bye")], &[Selection::new(0, 5)], t);
        assert_eq!(b.rope().to_string(), "bye world");

        let (_, sel) = b.undo().unwrap();
        assert_eq!(b.rope().to_string(), "hello world");
        assert_eq!(sel, vec![Selection::new(0, 5)]);
        let (_, sel) = b.undo().unwrap();
        assert_eq!(b.rope().to_string(), "hello");
        assert_eq!(sel, before.to_vec());
        assert!(b.undo().is_none());

        let (_, sel) = b.redo().unwrap();
        assert_eq!(b.rope().to_string(), "hello world");
        assert_eq!(sel, vec![Selection::cursor(11)]);
        b.redo().unwrap();
        assert_eq!(b.rope().to_string(), "bye world");
    }

    #[test]
    fn multi_edit_undo_redo() {
        let mut b = Buffer::new("a b c");
        b.edit([(0..1, "xx"), (4..5, "zz"), (2..3, "yy")], &[], now());
        b.undo().unwrap();
        assert_eq!(b.rope().to_string(), "a b c");
        b.redo().unwrap();
        assert_eq!(b.rope().to_string(), "xx yy zz");
    }

    #[test]
    fn fast_typing_groups_into_one_undo_step() {
        let t = now();
        let mut b = Buffer::new("");
        b.edit([(0..0, "a")], &[], t);
        b.edit([(1..1, "b")], &[], t + Duration::from_millis(50));
        b.edit([(2..2, "c")], &[], t + Duration::from_millis(100));
        b.edit([(3..3, "d")], &[], t + Duration::from_secs(5));
        b.undo();
        assert_eq!(b.rope().to_string(), "abc");
        b.undo();
        assert_eq!(b.rope().to_string(), "");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut b = Buffer::new("");
        b.edit([(0..0, "a")], &[], now());
        b.undo();
        b.edit([(0..0, "b")], &[], now());
        assert!(b.redo().is_none());
        assert_eq!(b.rope().to_string(), "b");
    }

    #[test]
    fn dirty_tracking() {
        let mut b = Buffer::new("a");
        assert!(!b.is_dirty());
        b.edit([(1..1, "b")], &[], now());
        assert!(b.is_dirty());
        b.mark_saved();
        assert!(!b.is_dirty());
    }
}
