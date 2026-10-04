//! How deep a line goes in a language whose extension says so: its
//! `indents.scm` and the two patterns of its `config.toml`. A language that
//! says nothing gets `None` here and the editor's own rule.

use std::{
    collections::HashMap,
    ops::Range,
    sync::{Arc, Mutex},
};

use regex::Regex;
use syntax::{Line, SyntaxTree};

use crate::document::Document;

/// The indentation a line should have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wanted {
    pub text: String,
    /// Worked out in a part of the file that does not parse.
    pub in_error: bool,
}

/// A pattern of a language, compiled once. One that does not compile is
/// remembered as that, and counts as never matching.
fn pattern(source: &str) -> Option<Arc<Regex>> {
    static KNOWN: Mutex<Option<HashMap<String, Option<Arc<Regex>>>>> = Mutex::new(None);
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    known
        .get_or_insert_with(HashMap::new)
        .entry(source.to_string())
        .or_insert_with(|| Regex::new(source).ok().map(Arc::new))
        .clone()
}

/// The tree, if it is in step with the text and the language at `offset`
/// has anything to say.
fn tree(doc: &Document, offset: usize) -> Option<&SyntaxTree> {
    doc.syntax()
        .filter(|syntax| !syntax.is_stale() && syntax.indents_at(offset))
}

fn blanks(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

fn wanted(doc: &Document, syntax: &SyntaxTree, line: Line, above: &str, own: &str) -> Wanted {
    let language = syntax.language_at(line.cut);
    let editing = language.editing();
    let matches = |source: Option<&String>, text: &str| {
        source
            .and_then(|source| pattern(source))
            .is_some_and(|pattern| pattern.is_match(text))
    };
    let increase = matches(editing.and_then(|e| e.increase_indent.as_ref()), above);
    let decrease = matches(editing.and_then(|e| e.decrease_indent.as_ref()), own);
    let indent = syntax.indent(doc.text().rope(), line, increase, decrease);
    let base = doc.text().line_str(indent.row);
    let mut text = base[..blanks(&base)].to_string();
    let unit = doc.indent_unit();
    match indent.levels {
        1 => text.push_str(unit),
        -1 => {
            let less = match text.strip_suffix(unit) {
                Some(less) => less.len(),
                // Not a whole level there: a tab, or what spaces there are.
                None if text.ends_with('\t') => text.len() - 1,
                None => text
                    .trim_end_matches(' ')
                    .len()
                    .max(text.len().saturating_sub(unit.len())),
            };
            text.truncate(less);
        }
        _ => {}
    }
    Wanted {
        text,
        in_error: indent.in_error,
    }
}

/// The indentation of the line Enter makes when it is typed over `range`.
pub fn after_break(doc: &Document, range: Range<usize>) -> Option<String> {
    let syntax = tree(doc, range.start)?;
    let buffer = doc.text();
    let cut = buffer.offset_to_point(range.start);
    let line = buffer.line_str(cut.row);
    let above = &line[..cut.column];
    // In the blanks before a line's text there is no row above to go by.
    if above.trim().is_empty() {
        return None;
    }
    let end = buffer.offset_to_point(range.end);
    let rest = buffer.line_str(end.row);
    let rest = &rest[end.column..];
    let at = Line {
        above_row: cut.row,
        above_start: buffer.line_start(cut.row) + blanks(above),
        cut: range.start,
        start: range.end + blanks(rest),
    };
    Some(wanted(doc, syntax, at, above, rest).text)
}

/// The indentation of `row` as the file stands.
pub fn of_row(doc: &Document, row: usize) -> Option<Wanted> {
    let buffer = doc.text();
    let start = buffer.line_start(row);
    let syntax = tree(doc, start)?;
    let above_row = (0..row)
        .rev()
        .find(|row| !buffer.line_str(*row).trim().is_empty())?;
    let above = buffer.line_str(above_row);
    let own = buffer.line_str(row);
    let at = Line {
        above_row,
        above_start: buffer.line_start(above_row) + blanks(&above),
        cut: start,
        start: start + blanks(&own),
    };
    Some(wanted(doc, syntax, at, &above, &own))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_that_does_not_compile_never_matches() {
        assert!(pattern(r"^\s*end\b").is_some_and(|p| p.is_match("  end")));
        assert!(pattern("(").is_none());
        // Asked again, it is the one kept.
        let first = pattern(r":\s*$").unwrap();
        assert!(Arc::ptr_eq(&first, &pattern(r":\s*$").unwrap()));
    }
}
