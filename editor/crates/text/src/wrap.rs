//! Where a line too long for the window goes on in the next row.
//!
//! A line is wrapped by cells, not by what its letters measure: every
//! character is one cell and a tab runs to the next stop. So how many rows
//! a line takes is known from its text alone, for every line of a file
//! and not only for the ones on screen, and what is drawn agrees with it.

use ropey::Rope;

use crate::TAB_SIZE;

/// The narrowest a text is wrapped to, however small the window.
const NARROWEST: usize = 8;

fn width(c: char, cells: usize) -> usize {
    match c {
        '\t' => TAB_SIZE - cells % TAB_SIZE,
        _ => 1,
    }
}

/// How many cells the rows of a line after its first leave empty before
/// their text: as many as the line itself is indented by, so that what
/// goes on is seen to belong to it, and never more than half the width.
pub fn hang(line: &str, cols: usize) -> usize {
    let mut cells = 0;
    for c in line.chars() {
        match c {
            ' ' | '\t' => cells += width(c, cells),
            _ => break,
        }
    }
    cells.min(cols.max(NARROWEST) / 2)
}

/// Where a line goes on in a new row when it is `cols` cells wide: the
/// bytes at which its rows after the first begin. A row ends after the
/// last space or tab that fits in it, and inside a word where the word
/// alone is longer than a row.
pub fn wrap_points(line: &str, cols: usize) -> Vec<usize> {
    let cols = cols.max(NARROWEST);
    let later = cols - hang(line, cols);
    let mut points = Vec::new();
    let (mut start, mut room) = (0, cols);
    loop {
        let (mut cells, mut space, mut end) = (0, None, None);
        // The spaces a row begins with are no place to end it: a row of
        // nothing but the line's indent would say nothing.
        let mut begun = false;
        for (at, c) in line[start..].char_indices() {
            let wide = width(c, cells);
            if cells + wide > room && at > 0 {
                end = Some(start + space.unwrap_or(at));
                break;
            }
            cells += wide;
            if c == ' ' || c == '\t' {
                if begun {
                    space = Some(at + c.len_utf8());
                }
            } else {
                begun = true;
            }
        }
        match end {
            Some(at) => {
                points.push(at);
                (start, room) = (at, later);
            }
            None => return points,
        }
    }
}

/// The lines of a text that take more than one row at `cols` cells, each
/// with how many more, in order. A line longer than `limit` bytes is
/// wrapped as far as that: the rest of it is not drawn either.
pub fn wrapped_lines(rope: &Rope, cols: usize, limit: usize) -> Vec<(usize, usize)> {
    let cols = cols.max(NARROWEST);
    // First the lines that are wider than a row at all, by their bytes:
    // most lines of most files are not, and are not looked at again.
    let mut long = Vec::new();
    let (mut row, mut cells) = (0, 0);
    for chunk in rope.chunks() {
        for byte in chunk.bytes() {
            match byte {
                b'\n' => {
                    if cells > cols {
                        long.push(row);
                    }
                    (row, cells) = (row + 1, 0);
                }
                b'\t' => cells += TAB_SIZE - cells % TAB_SIZE,
                // Not the later bytes of a character of several.
                0x80..=0xBF => {}
                _ => cells += 1,
            }
        }
    }
    if cells > cols {
        long.push(row);
    }
    let rows = |row: usize| {
        let line = rope.line(row);
        let line: std::borrow::Cow<str> = line.into();
        let line = line.trim_end_matches('\n');
        let mut end = line.len().min(limit);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        (row, wrap_points(&line[..end], cols).len())
    };
    long.into_iter()
        .map(rows)
        .filter(|(_, more)| *more > 0)
        .collect()
}

impl crate::Buffer {
    /// Moves up or down by rows of the screen in a text wrapped at `cols`
    /// cells: within a line that takes several rows, from one of them to
    /// the next. The goal is how far along a row the cursor wants to be,
    /// in cells from the left of the text. `limit` is how much of a line
    /// is drawn.
    pub fn move_wrapped(
        &self,
        offset: usize,
        goal: crate::SelectionGoal,
        rows: isize,
        cols: usize,
        limit: usize,
    ) -> (usize, crate::SelectionGoal) {
        let cells_of = |text: &str| text.chars().fold(0, |cells, c| cells + width(c, cells));
        // A line as its rows: where each begins, and the line's text.
        let parts = |row: usize| {
            let line = self.line_str(row).into_owned();
            let mut end = line.len().min(limit);
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            let mut starts = vec![0];
            starts.extend(wrap_points(&line[..end], cols));
            (line, starts)
        };
        let point = self.offset_to_point(offset);
        let (mut row, (mut line, mut starts)) = (point.row, parts(point.row));
        let mut part = starts.partition_point(|start| *start <= point.column) - 1;
        let hung = |line: &str, part: usize| match part {
            0 => 0,
            _ => hang(line, cols),
        };
        let goal_cells = match goal {
            crate::SelectionGoal::Column(cells) => cells,
            crate::SelectionGoal::None => {
                let before = &line[starts[part]..point.column.min(line.len())];
                hung(&line, part) + cells_of(before)
            }
        };
        let goal = crate::SelectionGoal::Column(goal_cells);
        for _ in 0..rows.unsigned_abs() {
            if rows > 0 {
                if part + 1 < starts.len() {
                    part += 1;
                } else if row + 1 < self.line_count() {
                    row += 1;
                    (line, starts) = parts(row);
                    part = 0;
                } else {
                    return (self.len(), goal);
                }
            } else if part > 0 {
                part -= 1;
            } else if row > 0 {
                row -= 1;
                (line, starts) = parts(row);
                part = starts.len() - 1;
            } else {
                return (0, goal);
            }
        }
        // The place in that row nearest to the goal. The last cell of a
        // row that goes on is the first of the next, so it is not gone to.
        let from = starts[part];
        let to = starts.get(part + 1).copied().unwrap_or(line.len());
        let wanted = goal_cells.saturating_sub(hung(&line, part));
        let (mut column, mut cells) = (from, 0);
        for (at, c) in line[from..to].char_indices() {
            let wide = width(c, cells);
            if cells + wide > wanted {
                break;
            }
            cells += wide;
            column = from + at + c.len_utf8();
        }
        if column >= to && part + 1 < starts.len() {
            column = self.prev_grapheme(self.line_start(row) + to) - self.line_start(row);
        }
        (self.line_start(row) + column.max(from), goal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line as its rows, by where it is wrapped.
    fn rows(line: &str, cols: usize) -> Vec<&str> {
        let mut out = Vec::new();
        let mut from = 0;
        for at in wrap_points(line, cols) {
            out.push(&line[from..at]);
            from = at;
        }
        out.push(&line[from..]);
        out
    }

    #[test]
    fn a_line_goes_on_after_the_last_space_that_fits() {
        assert_eq!(rows("short", 10), ["short"]);
        assert_eq!(rows("exactly 10", 10), ["exactly 10"]);
        assert_eq!(
            rows("the quick brown fox jumps", 10),
            ["the quick ", "brown fox ", "jumps"]
        );
        // A word longer than a row is cut where the row ends.
        assert_eq!(
            rows("a supercalifragilistic word", 10),
            ["a ", "supercalif", "ragilistic", " word"]
        );
        // Letters of several bytes are a cell each, and are never cut.
        assert_eq!(rows("привет мир и все", 10), ["привет ", "мир и все"]);
        assert_eq!(rows("ééééééééééééé", 10), ["éééééééééé", "ééé"]);
        // However narrow the window, some of the line fits in a row.
        assert_eq!(rows("abcdefghij", 1), ["abcdefgh", "ij"]);
        assert!(wrap_points("", 10).is_empty());
    }

    #[test]
    fn what_goes_on_is_as_far_in_as_the_line_begins() {
        // Indented by four, the rows after the first have four cells
        // less: they are drawn as far in as the first.
        assert_eq!(hang("    let x = 1;", 20), 4);
        assert_eq!(
            rows("    let total = first + second + third;", 20),
            ["    let total = ", "first + second ", "+ third;"]
        );
        // A tab is as wide as to its next stop; an indent deeper than
        // half the width leaves half the width.
        assert_eq!(hang("\tx", 20), TAB_SIZE);
        assert_eq!(hang("                    deep", 20), 10);
        // The line's own indent is no place to go on at: a long word
        // after it is cut, and the first row is not the indent alone.
        assert_eq!(rows("    indented", 10), ["    indent", "ed"]);
        assert_eq!(
            rows("\tone two three four", 12),
            ["\tone two ", "three ", "four"]
        );
    }

    #[test]
    fn the_cursor_moves_by_rows_of_the_screen() {
        use crate::{Buffer, SelectionGoal};
        // At ten cells: "the quick " / "brown fox " / "jumps", then a
        // short line, then "    ab cd " / "ef gh " / "ij", the last two
        // drawn four cells in.
        let buffer = Buffer::new("the quick brown fox jumps\nend\n    ab cd ef gh ij\n");
        let down = |offset: usize, goal: SelectionGoal| {
            buffer.move_wrapped(offset, goal, 1, 10, usize::MAX)
        };
        let up = |offset: usize, goal: SelectionGoal| {
            buffer.move_wrapped(offset, goal, -1, 10, usize::MAX)
        };
        // From the fourth cell of the first row, down through the rows
        // of the one line, keeping to the fourth cell where a row has it.
        let (at, goal) = down(3, SelectionGoal::None);
        assert_eq!((at, goal), (13, SelectionGoal::Column(3)));
        let (at, goal) = down(at, goal);
        assert_eq!(at, 23);
        // Then the next line, which is shorter than the goal.
        let (at, goal) = down(at, goal);
        assert_eq!(at, 29);
        // Into the line that begins four cells in: the goal is counted
        // from the left of the text, so it is inside the line's indent.
        let (at, goal) = down(at, goal);
        assert_eq!(at, 33);
        // Its second row is drawn four cells in: nothing of it is as far
        // left as the goal, so its start is the nearest.
        let (at, goal) = down(at, goal);
        assert_eq!(at, 40);
        // And back up the same way.
        let (at, goal) = up(at, goal);
        assert_eq!(at, 33);
        let (at, goal) = up(at, goal);
        assert_eq!(at, 29);
        let (at, goal) = up(at, goal);
        assert_eq!(at, 23);
        let (at, _) = up(at, goal);
        assert_eq!(at, 13);
        // From the end of a row that goes on, down stays at the end of
        // the rows below and never lands on the row after.
        let (at, goal) = down(9, SelectionGoal::None);
        assert_eq!(at, 19);
        assert_eq!(down(at, goal).0, 25);
        // Past either end of the text is that end.
        assert_eq!(up(3, SelectionGoal::None).0, 0);
        assert_eq!(
            buffer
                .move_wrapped(40, SelectionGoal::None, 5, 10, usize::MAX)
                .0,
            buffer.len()
        );
        // Several rows at once are that many steps.
        assert_eq!(
            buffer
                .move_wrapped(3, SelectionGoal::None, 3, 10, usize::MAX)
                .0,
            29
        );
    }

    #[test]
    fn the_lines_of_a_text_that_take_more_rows_are_counted() {
        let text = "short\nthe quick brown fox jumps\n\nпривет мир и все\n\tone two three four\nlast line is long too";
        let rope = Rope::from_str(text);
        assert_eq!(
            wrapped_lines(&rope, 10, usize::MAX),
            [(1, 2), (3, 1), (4, 3), (5, 2)]
        );
        // What it says of each line is what wrapping that line gives.
        for (row, more) in wrapped_lines(&rope, 12, usize::MAX) {
            let line = text.split('\n').nth(row).unwrap();
            assert_eq!(wrap_points(line, 12).len(), more, "{line:?}");
        }
        // Nothing is wider than a wide window; a line cut short is
        // wrapped only as far as it is drawn.
        assert!(wrapped_lines(&rope, 80, usize::MAX).is_empty());
        assert_eq!(wrapped_lines(&rope, 10, 12), [(1, 1), (4, 2), (5, 1)]);
    }
}
