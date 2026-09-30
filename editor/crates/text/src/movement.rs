use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use crate::{Buffer, Point};

/// Where vertical movement tries to land, in display columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionGoal {
    #[default]
    None,
    Column(usize),
}

/// `anchor` stays put while extending; `head` is where the caret is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    pub goal: SelectionGoal,
}

impl Selection {
    pub fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal: SelectionGoal::None,
        }
    }

    pub fn cursor(offset: usize) -> Self {
        Self::new(offset, offset)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_reversed(&self) -> bool {
        self.head < self.anchor
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

pub const TAB_SIZE: usize = 4;

impl Buffer {
    pub fn prev_grapheme(&self, offset: usize) -> usize {
        let p = self.offset_to_point(offset);
        if p.column == 0 {
            return offset.saturating_sub(1);
        }
        let line = self.line_str(p.row);
        let col = line[..p.column]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i);
        self.line_start(p.row) + col
    }

    pub fn next_grapheme(&self, offset: usize) -> usize {
        let p = self.offset_to_point(offset);
        let line = self.line_str(p.row);
        if p.column >= line.len() {
            return (offset + 1).min(self.len());
        }
        let len = line[p.column..].graphemes(true).next().map_or(0, str::len);
        offset + len
    }

    /// Skips whitespace backwards, then a run of one character class.
    pub fn prev_word_start(&self, offset: usize) -> usize {
        let rope = self.rope();
        let mut idx = rope.byte_to_char(self.clip_offset(offset));
        while idx > 0 && class(rope.char(idx - 1)) == CharClass::Space {
            idx -= 1;
        }
        if idx > 0 {
            let run = class(rope.char(idx - 1));
            while idx > 0 && class(rope.char(idx - 1)) == run {
                idx -= 1;
            }
        }
        rope.char_to_byte(idx)
    }

    /// Skips whitespace forwards, then a run of one character class.
    pub fn next_word_end(&self, offset: usize) -> usize {
        let rope = self.rope();
        let len = rope.len_chars();
        let mut idx = rope.byte_to_char(self.clip_offset(offset));
        while idx < len && class(rope.char(idx)) == CharClass::Space {
            idx += 1;
        }
        if idx < len {
            let run = class(rope.char(idx));
            while idx < len && class(rope.char(idx)) == run {
                idx += 1;
            }
        }
        rope.char_to_byte(idx)
    }

    /// The word (or single punctuation run) around `offset`, for double-click.
    pub fn word_range_at(&self, offset: usize) -> Range<usize> {
        let rope = self.rope();
        let len = rope.len_chars();
        let idx = rope.byte_to_char(self.clip_offset(offset));
        let probe = if idx < len && rope.char(idx) != '\n' {
            idx
        } else if idx > 0 {
            idx - 1
        } else {
            return offset..offset;
        };
        let run = class(rope.char(probe));
        let mut start = probe;
        while start > 0 && rope.char(start - 1) != '\n' && class(rope.char(start - 1)) == run {
            start -= 1;
        }
        let mut end = probe;
        while end < len && rope.char(end) != '\n' && class(rope.char(end)) == run {
            end += 1;
        }
        rope.char_to_byte(start)..rope.char_to_byte(end)
    }

    pub fn line_range_at(&self, offset: usize) -> Range<usize> {
        let row = self.offset_to_point(offset).row;
        let start = self.line_start(row);
        let end = if row + 1 < self.line_count() {
            self.line_start(row + 1)
        } else {
            self.len()
        };
        start..end
    }

    /// Home toggles between the first non-blank character and column 0.
    pub fn smart_home(&self, offset: usize) -> usize {
        let p = self.offset_to_point(offset);
        let line = self.line_str(p.row);
        let indent = line.len() - line.trim_start().len();
        let start = self.line_start(p.row);
        if p.column == indent {
            start
        } else {
            start + indent
        }
    }

    pub fn line_end(&self, offset: usize) -> usize {
        let row = self.offset_to_point(offset).row;
        self.line_start(row) + self.line_len(row)
    }

    /// Column as drawn on screen: tabs advance to the next tab stop and each
    /// grapheme takes one cell.
    pub fn display_column(&self, point: Point) -> usize {
        let line = self.line_str(point.row);
        let prefix = &line[..point.column.min(line.len())];
        let mut col = 0;
        for g in prefix.graphemes(true) {
            col += if g == "\t" {
                TAB_SIZE - col % TAB_SIZE
            } else {
                1
            };
        }
        col
    }

    /// Inverse of [`Buffer::display_column`], landing on the nearest grapheme
    /// boundary at or before `target`.
    pub fn column_for_display(&self, row: usize, target: usize) -> usize {
        let line = self.line_str(row);
        let mut col = 0;
        for (i, g) in line.grapheme_indices(true) {
            let width = if g == "\t" {
                TAB_SIZE - col % TAB_SIZE
            } else {
                1
            };
            if col + width > target {
                return i;
            }
            col += width;
        }
        line.len()
    }

    /// The bracket at or just before `offset` and its partner, as byte offsets
    /// of the two bracket characters. Scans at most `limit` chars each way.
    pub fn matching_bracket(&self, offset: usize, limit: usize) -> Option<(usize, usize)> {
        let rope = self.rope();
        let idx = rope.byte_to_char(self.clip_offset(offset));
        let candidates = [Some(idx), idx.checked_sub(1)];
        for at in candidates.into_iter().flatten() {
            if at >= rope.len_chars() {
                continue;
            }
            let c = rope.char(at);
            let (open, close, forward) = match c {
                '(' => ('(', ')', true),
                '[' => ('[', ']', true),
                '{' => ('{', '}', true),
                ')' => ('(', ')', false),
                ']' => ('[', ']', false),
                '}' => ('{', '}', false),
                _ => continue,
            };
            let mut depth = 0usize;
            let mut i = at;
            for _ in 0..limit {
                let ch = rope.char(i);
                if ch == open {
                    depth = if forward {
                        depth + 1
                    } else {
                        depth.checked_sub(1)?
                    };
                } else if ch == close {
                    depth = if forward {
                        depth.checked_sub(1)?
                    } else {
                        depth + 1
                    };
                }
                if depth == 0 {
                    let (a, b) = (rope.char_to_byte(at), rope.char_to_byte(i));
                    return Some((a.min(b), a.max(b)));
                }
                if forward {
                    i += 1;
                    if i >= rope.len_chars() {
                        return None;
                    }
                } else {
                    i = i.checked_sub(1)?;
                }
            }
            return None;
        }
        None
    }

    /// Moves `rows` up (negative) or down, keeping the goal column. Moving past
    /// the first or last row jumps to the start or end of the buffer.
    pub fn move_vertically(
        &self,
        offset: usize,
        goal: SelectionGoal,
        rows: isize,
    ) -> (usize, SelectionGoal) {
        let p = self.offset_to_point(offset);
        let goal_col = match goal {
            SelectionGoal::Column(c) => c,
            SelectionGoal::None => self.display_column(p),
        };
        let goal = SelectionGoal::Column(goal_col);
        let target = p.row as isize + rows;
        if target < 0 {
            return (0, goal);
        }
        let target = target as usize;
        if target >= self.line_count() {
            return (self.len(), goal);
        }
        let column = self.column_for_display(target, goal_col);
        (self.line_start(target) + column, goal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphemes_step_over_combining_marks() {
        let b = Buffer::new("e\u{301}x\ny");
        assert_eq!(b.next_grapheme(0), 3);
        assert_eq!(b.prev_grapheme(3), 0);
        // Crossing a newline moves by one byte.
        assert_eq!(b.next_grapheme(4), 5);
        assert_eq!(b.prev_grapheme(5), 4);
        assert_eq!(b.next_grapheme(b.len()), b.len());
        assert_eq!(b.prev_grapheme(0), 0);
    }

    #[test]
    fn word_motion() {
        let b = Buffer::new("let foo_bar = baz();");
        assert_eq!(b.next_word_end(0), 3);
        assert_eq!(b.next_word_end(3), 11);
        assert_eq!(b.next_word_end(11), 13);
        assert_eq!(b.prev_word_start(11), 4);
        assert_eq!(b.prev_word_start(20), 17);
        assert_eq!(b.word_range_at(6), 4..11);
    }

    #[test]
    fn smart_home_toggles() {
        let b = Buffer::new("x\n    hello");
        let end = b.len();
        assert_eq!(b.smart_home(end), 6);
        assert_eq!(b.smart_home(6), 2);
        assert_eq!(b.smart_home(2), 6);
    }

    #[test]
    fn vertical_motion_keeps_goal_column() {
        let b = Buffer::new("abcdef\nab\nabcdef");
        let (o, g) = b.move_vertically(5, SelectionGoal::None, 1);
        assert_eq!(o, 9);
        let (o, _) = b.move_vertically(o, g, 1);
        assert_eq!(o, 15);
        assert_eq!(b.move_vertically(3, SelectionGoal::None, -1).0, 0);
        assert_eq!(b.move_vertically(12, SelectionGoal::None, 1).0, b.len());
    }

    #[test]
    fn brackets_match_both_ways() {
        let b = Buffer::new("f(a[0], {x}) y");
        assert_eq!(b.matching_bracket(1, 100), Some((1, 11)));
        assert_eq!(b.matching_bracket(12, 100), Some((1, 11)));
        assert_eq!(b.matching_bracket(3, 100), Some((3, 5)));
        assert_eq!(b.matching_bracket(13, 100), None);
        assert_eq!(b.matching_bracket(1, 3), None);
    }

    #[test]
    fn tabs_expand_to_stops() {
        let b = Buffer::new("\tab\n  \tc");
        assert_eq!(b.display_column(Point::new(0, 1)), 4);
        assert_eq!(b.display_column(Point::new(1, 3)), 4);
        assert_eq!(b.column_for_display(0, 2), 0);
        assert_eq!(b.column_for_display(0, 5), 2);
    }
}
