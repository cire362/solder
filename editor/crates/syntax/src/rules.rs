//! What an extension says about its language besides colors: the pairs that
//! close themselves, the comment that has an end, and the queries for
//! brackets, indentation, scopes and the outline. A built-in language has
//! none of it and the editor keeps to its own rules there.

use std::{ops::Range, path::PathBuf};

use text::Rope;
use tree_sitter::{Query, QueryCursor, StreamingIterator};

use super::{Language, RopeProvider, Source, SyntaxTree};

/// Two pieces of text that go together, from `brackets` in `config.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pair {
    pub start: String,
    pub end: String,
    /// The end is typed for the user when the start is.
    pub close: bool,
    /// Enter between the two puts the end on a line of its own.
    pub newline: bool,
    /// Scopes of `overrides.scm` in which the pair does not close.
    pub not_in: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Editing {
    pub pairs: Vec<Pair>,
    /// What a pair may close in front of, besides a blank.
    pub autoclose_before: Option<String>,
    /// A comment with a start and an end, for languages with no other kind.
    pub block_comment: Option<(String, String)>,
    /// Characters that belong to a word besides letters, digits and `_`.
    pub word_characters: String,
    /// The same for the word a completion goes on from.
    pub completion_characters: String,
    /// Regular expressions, kept as written: a line that matches the first
    /// is followed by a deeper one, a line that matches the second goes one
    /// level back.
    pub increase_indent: Option<String>,
    pub decrease_indent: Option<String>,
    pub indents: Option<PathBuf>,
    pub brackets: Option<PathBuf>,
    pub outline: Option<PathBuf>,
    pub overrides: Option<PathBuf>,
}

/// The compiled queries of one language.
#[derive(Default)]
pub(crate) struct Rules {
    brackets: Option<Brackets>,
    indents: Option<Indents>,
    overrides: Option<Overrides>,
    pub(crate) outline: Option<Outline>,
}

struct Brackets {
    query: Query,
    open: u32,
    close: u32,
}

struct Indents {
    query: Query,
    indent: u32,
    start: Option<u32>,
    end: Option<u32>,
    outdent: Option<u32>,
}

struct Overrides {
    query: Query,
    /// For each capture, the scope's name and whether its edges count.
    scopes: Vec<(String, bool)>,
}

pub(crate) struct Outline {
    pub(crate) query: Query,
    pub(crate) item: u32,
    pub(crate) name: u32,
    pub(crate) context: Option<u32>,
}

impl Rules {
    pub(crate) fn load(language: &Language) -> Self {
        let Source::Wasm(spec) = &language.source else {
            return Self::default();
        };
        let editing = &spec.editing;
        let query = |path: &Option<PathBuf>, what: &str| {
            let source = std::fs::read_to_string(path.as_ref()?).ok()?;
            language.query(&source, what)
        };
        Self {
            brackets: query(&editing.brackets, "brackets").and_then(|query| {
                Some(Brackets {
                    open: query.capture_index_for_name("open")?,
                    close: query.capture_index_for_name("close")?,
                    query,
                })
            }),
            indents: query(&editing.indents, "indents").and_then(|query| {
                Some(Indents {
                    indent: query.capture_index_for_name("indent")?,
                    start: query.capture_index_for_name("start"),
                    end: query.capture_index_for_name("end"),
                    outdent: query.capture_index_for_name("outdent"),
                    query,
                })
            }),
            overrides: query(&editing.overrides, "overrides").map(|query| Overrides {
                scopes: query
                    .capture_names()
                    .iter()
                    .map(|name| match name.strip_suffix(".inclusive") {
                        Some(name) => (name.to_string(), true),
                        None => (name.to_string(), false),
                    })
                    .collect(),
                query,
            }),
            outline: query(&editing.outline, "outline").and_then(|query| {
                Some(Outline {
                    item: query.capture_index_for_name("item")?,
                    name: query.capture_index_for_name("name")?,
                    context: query.capture_index_for_name("context"),
                    query,
                })
            }),
        }
    }
}

/// A line that is about to get its indentation, in the text as the tree
/// knows it: either a line that is there, or the one Enter is about to make.
#[derive(Clone, Copy, Debug)]
pub struct Line {
    /// The nearest row above that has text, and where that text starts.
    pub above_row: usize,
    pub above_start: usize,
    /// Where the row above stops: the line's own start, or the cursor when
    /// Enter breaks a row in two.
    pub cut: usize,
    /// Where the text of the line itself starts.
    pub start: usize,
}

/// As deep as `row`, plus or minus one level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Indent {
    pub row: usize,
    pub levels: i8,
    /// The line is in a part of the file that does not parse, so the answer
    /// is a guess: good for a new line, not for moving one that is there.
    pub in_error: bool,
}

impl SyntaxTree {
    /// Whether the language at `offset` has a say on indentation at all.
    pub fn indents_at(&self, offset: usize) -> bool {
        let (language, _) = self.tree_at(offset);
        language.rules().indents.is_some()
            || language
                .editing()
                .is_some_and(|e| e.increase_indent.is_some() || e.decrease_indent.is_some())
    }

    /// How deep `line` goes. `increase` and `decrease` are the answers of the
    /// language's two patterns (see [`Editing`]) for the row above and for
    /// the line. The rules are Zed's, so an extension's query means here
    /// what it means there.
    pub fn indent(&self, rope: &Rope, line: Line, increase: bool, decrease: bool) -> Indent {
        let above = line.above_row;
        let mut from_above = increase;
        let mut back_to: Option<usize> = None;
        let (language, tree) = self.tree_at(line.cut);
        if let Some(indents) = &language.rules().indents {
            // A range starts and ends at a byte. One that is the end of a
            // node stays with the row above when the cut falls right on it;
            // one that is the start of a node goes with the line.
            struct Range {
                start: (usize, bool),
                row: usize,
                end: (usize, bool),
            }
            let mut ranges: Vec<Range> = Vec::new();
            let mut outdents = Vec::new();
            let mut cursor = QueryCursor::new();
            cursor.set_match_limit(256);
            cursor.set_byte_range(line.above_start..(line.start + 1).min(rope.len_bytes()));
            let mut matches = cursor.matches(&indents.query, tree.root_node(), RopeProvider(rope));
            while let Some(m) = matches.next() {
                let (mut start, mut end) = (None, None);
                for capture in m.captures {
                    let node = capture.node;
                    let index = Some(capture.index);
                    if capture.index == indents.indent {
                        start
                            .get_or_insert(((node.start_byte(), false), node.start_position().row));
                        end.get_or_insert((node.end_byte(), true));
                    } else if index == indents.start {
                        start = Some(((node.end_byte(), true), node.end_position().row));
                    } else if index == indents.end {
                        end = Some((node.start_byte(), false));
                    } else if index == indents.outdent {
                        outdents.push(node.start_byte());
                    }
                }
                if let (Some((start, row)), Some(end)) = (start, end) {
                    ranges.push(Range { start, row, end });
                }
            }
            // An outdent ends the innermost range around it.
            for at in outdents {
                if let Some(range) = ranges
                    .iter_mut()
                    .filter(|r| r.start.0 <= at && at < r.end.0)
                    .min_by_key(|r| r.end.0 - r.start.0)
                {
                    range.end = (at, false);
                }
            }
            let above_cut =
                |(at, end_of_node): (usize, bool)| at < line.cut || (at == line.cut && end_of_node);
            for range in ranges {
                let end = range.end.0;
                // It starts on the line itself, or it is one row long.
                if !above_cut(range.start)
                    || (above_cut(range.end) && rope.byte_to_line(end) == range.row)
                {
                    continue;
                }
                if range.row == above && !above_cut(range.end) && end > line.start {
                    from_above = true;
                }
                if end > line.above_start && (above_cut(range.end) || end <= line.start) {
                    back_to = Some(back_to.map_or(range.row, |known| known.min(range.row)));
                }
            }
        }
        let (row, levels) = if back_to == Some(above) || (decrease && from_above) {
            (above, 0)
        } else if from_above {
            (above, 1)
        } else if let Some(row) = back_to.filter(|row| *row < above) {
            (row, 0)
        } else if decrease {
            (above, -1)
        } else {
            (above, 0)
        };
        let mut in_error = false;
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(line.start, line.start);
        while let Some(parent) = node {
            if parent.is_error() && parent.start_byte() < line.cut && parent.end_byte() > line.start
            {
                in_error = true;
                break;
            }
            node = parent.parent();
        }
        Indent {
            row,
            levels,
            in_error,
        }
    }

    /// The bracket at `offset`, or the one that ends there, and its other
    /// half. The outer `None` says the language there has no query for
    /// brackets, and the caller is left with the characters.
    #[allow(clippy::type_complexity)] // two ranges, twice optional; a type would only name it
    pub fn brackets_at(
        &self,
        rope: &Rope,
        offset: usize,
    ) -> Option<Option<(Range<usize>, Range<usize>)>> {
        // At the edge of an injected language the bracket may belong to the
        // language around it: `{{` in a template, with a script inside.
        for (language, tree) in self.trees_at(offset) {
            let Some(brackets) = &language.rules().brackets else {
                continue;
            };
            let mut cursor = QueryCursor::new();
            cursor.set_match_limit(256);
            cursor.set_byte_range(offset.saturating_sub(1)..(offset + 1).min(rope.len_bytes()));
            let mut best: Option<((bool, usize), (Range<usize>, Range<usize>))> = None;
            let mut matches = cursor.matches(&brackets.query, tree.root_node(), RopeProvider(rope));
            while let Some(m) = matches.next() {
                let node = |index| m.nodes_for_capture_index(index).next();
                let (Some(open), Some(close)) = (node(brackets.open), node(brackets.close)) else {
                    continue;
                };
                let (open, close) = (open.byte_range(), close.byte_range());
                let at = |r: &Range<usize>| r.start <= offset && offset < r.end;
                let after = |r: &Range<usize>| r.end == offset;
                let behind = if at(&open) || at(&close) {
                    false
                } else if after(&open) || after(&close) {
                    true
                } else {
                    continue;
                };
                // The one under the cursor before the one behind it, then
                // the innermost.
                let rank = (behind, close.end - open.start);
                if best.as_ref().is_none_or(|(known, _)| rank < *known) {
                    best = Some((rank, (open, close)));
                }
            }
            if let Some((_, pair)) = best {
                return Some(Some(pair));
            }
        }
        let (language, _) = self.tree_at(offset);
        language.rules().brackets.as_ref().map(|_| None)
    }

    /// Whether `offset` is inside one of the scopes the language's
    /// `overrides.scm` gives names to: `string`, `comment`.
    pub fn in_scope(&self, rope: &Rope, offset: usize, scopes: &[String]) -> bool {
        let (language, tree) = self.tree_at(offset);
        let Some(overrides) = &language.rules().overrides else {
            return false;
        };
        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(64);
        cursor.set_byte_range(offset.saturating_sub(1)..(offset + 1).min(rope.len_bytes()));
        let mut captures = cursor.captures(&overrides.query, tree.root_node(), RopeProvider(rope));
        while let Some((m, index)) = captures.next() {
            let capture = m.captures[*index];
            let (name, edges) = &overrides.scopes[capture.index as usize];
            let range = capture.node.byte_range();
            let inside = if *edges {
                range.start <= offset && offset <= range.end
            } else {
                range.start < offset && offset < range.end
            };
            if inside && scopes.contains(name) {
                return true;
            }
        }
        false
    }
}
