//! Text positions in the emulator are cells, not byte offsets. Keep the
//! mapping while joining soft wraps so links still work after a resize.
use std::{ops::Range, path::PathBuf, sync::OnceLock};

use alacritty_terminal::{
    event::EventListener,
    grid::Dimensions,
    index::{Column, Line, Point},
    term::{
        Term,
        cell::{Flags, LineLength},
    },
};

#[derive(Clone, Debug, PartialEq)]
pub struct FileLink {
    pub path: PathBuf,
    pub row: usize,
    pub column: usize,
}

pub struct TextLine {
    pub text: String,
    pub cells: Vec<(Range<usize>, Point)>,
}

pub fn read_line<T: EventListener>(term: &Term<T>, row: Line) -> TextLine {
    let top = term.topmost_line().0;
    let bottom = term.bottommost_line().0;
    let last = Column(term.columns() - 1);
    let mut start = row.0;
    while start > top
        && row.0 - start < 128
        && term.grid()[Line(start - 1)][last]
            .flags
            .contains(Flags::WRAPLINE)
    {
        start -= 1;
    }
    let mut out = TextLine {
        text: String::new(),
        cells: Vec::new(),
    };
    for line in start..=bottom.min(start + 128) {
        let grid_row = &term.grid()[Line(line)];
        let wrapped = grid_row[last].flags.contains(Flags::WRAPLINE);
        let end = if wrapped {
            term.columns()
        } else {
            grid_row.line_length().0
        };
        for col in 0..end {
            let cell = &grid_row[Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                if let Some((range, _)) = out.cells.last() {
                    out.cells
                        .push((range.clone(), Point::new(Line(line), Column(col))));
                }
                continue;
            }
            if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let begin = out.text.len();
            out.text.push(cell.c);
            for c in cell.zerowidth().into_iter().flatten() {
                out.text.push(*c);
            }
            out.cells
                .push((begin..out.text.len(), Point::new(Line(line), Column(col))));
        }
        if !wrapped || out.text.len() >= 32_768 {
            break;
        }
    }
    out
}

pub fn links(text: &str) -> Vec<(Range<usize>, FileLink)> {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(r#"(?:"[^"\n]+"|'[^'\n]+'|[^\s"'<>(),;\[\]{}]+):[0-9]+(?::[0-9]+)?"#)
            .unwrap()
    });
    pattern
        .find_iter(text)
        .filter_map(|m| {
            let (before, last) = m.as_str().rsplit_once(':')?;
            let last: usize = last.parse().ok()?;
            let (path, row, column) = match before.rsplit_once(':') {
                Some((path, row)) if row.bytes().all(|c| c.is_ascii_digit()) && !row.is_empty() => {
                    (path, row.parse().ok()?, last)
                }
                _ => (before, last, 1),
            };
            let path = path.trim_matches(['\'', '"']);
            if row == 0 || column == 0 || path.is_empty() || path.contains("://") {
                return None;
            }
            Some((
                m.range(),
                FileLink {
                    path: path.into(),
                    row: row - 1,
                    column: column - 1,
                },
            ))
        })
        .collect()
}

impl TextLine {
    pub fn link_at(&self, point: Point) -> Option<FileLink> {
        let byte = self.cells.iter().find(|(_, cell)| *cell == point)?.0.start;
        links(&self.text)
            .into_iter()
            .find(|(range, _)| range.contains(&byte))
            .map(|(_, link)| link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_positions_keep_unicode_quotes_and_drive_colons() {
        let all = links(
            r#" --> src/ёж.rs:12:3, "not a path" "my file.rs":5:2 C:\work\main.rs:9 https://host:443 bad:0:1"#,
        );
        assert_eq!(
            all.iter().map(|(_, l)| l.clone()).collect::<Vec<_>>(),
            [
                FileLink {
                    path: "src/ёж.rs".into(),
                    row: 11,
                    column: 2
                },
                FileLink {
                    path: "my file.rs".into(),
                    row: 4,
                    column: 1
                },
                FileLink {
                    path: r"C:\work\main.rs".into(),
                    row: 8,
                    column: 0
                },
            ]
        );
        assert!(links("file:999999999999999999999999999:1").is_empty());
    }

    #[test]
    fn ansi_wrapped_links_map_back_to_wide_and_combining_cells() {
        struct Size;
        impl Dimensions for Size {
            fn total_lines(&self) -> usize {
                8
            }
            fn screen_lines(&self) -> usize {
                8
            }
            fn columns(&self) -> usize {
                8
            }
        }
        let mut term = Term::new(
            Default::default(),
            &Size,
            alacritty_terminal::event::VoidListener,
        );
        let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
        parser.advance(
            &mut term,
            "\x1b[31msrc/界e\u{301}.rs:2:3\x1b[0m\r\n".as_bytes(),
        );
        let row = read_line(&term, Line(1));
        assert_eq!(row.text, "src/界e\u{301}.rs:2:3");
        for (_, cell) in &row.cells {
            assert_eq!(
                row.link_at(*cell).unwrap().path,
                PathBuf::from("src/界e\u{301}.rs")
            );
        }
    }
}
