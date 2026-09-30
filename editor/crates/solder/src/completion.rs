//! The completion menu's state and snippet expansion. Requests and
//! keyboard handling live in the editor.

use std::{ops::Range, sync::Arc};

use gpui::{Hsla, UniformListScrollHandle};
use lsp::{Encoding, types as lt};

use crate::{fuzzy, theme::Theme};

pub struct CompletionMenu {
    pub items: Arc<Vec<lt::CompletionItem>>,
    pub encoding: Encoding,
    /// Indices into `items` that match what has been typed, best first, with
    /// the matched character positions in the label.
    pub filtered: Vec<(usize, Vec<u32>)>,
    pub selected: usize,
    /// Byte offset where the word being completed starts.
    pub word_start: usize,
    pub scroll: UniformListScrollHandle,
}

impl CompletionMenu {
    pub fn new(items: Vec<lt::CompletionItem>, encoding: Encoding, word_start: usize) -> Self {
        let mut items = items;
        // Server order is only a hint; sort_text is the contract.
        items.sort_by(|a, b| {
            let ka = a.sort_text.as_deref().unwrap_or(&a.label);
            let kb = b.sort_text.as_deref().unwrap_or(&b.label);
            ka.cmp(kb)
        });
        Self {
            items: Arc::new(items),
            encoding,
            filtered: Vec::new(),
            selected: 0,
            word_start,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Filters by the typed word. Returns false when nothing matches.
    pub fn filter(&mut self, query: &str) -> bool {
        let keys: Vec<&str> = self
            .items
            .iter()
            .map(|i| i.filter_text.as_deref().unwrap_or(&i.label))
            .collect();
        self.filtered = if query.is_empty() {
            (0..self.items.len()).map(|i| (i, Vec::new())).collect()
        } else {
            fuzzy::fuzzy_match(keys.iter().copied(), query, 200, false)
                .into_iter()
                .map(|m| {
                    let label = &self.items[m.index].label;
                    (m.index, fuzzy::positions(label, query, false))
                })
                .collect()
        };
        self.selected = 0;
        !self.filtered.is_empty()
    }

    pub fn selected_item(&self) -> Option<&lt::CompletionItem> {
        self.filtered
            .get(self.selected)
            .map(|(i, _)| &self.items[*i])
    }
}

/// A one-letter badge and its color for a completion kind.
pub fn kind_badge(kind: Option<lt::CompletionItemKind>, theme: &Theme) -> (&'static str, Hsla) {
    use lt::CompletionItemKind as K;
    match kind {
        Some(K::FUNCTION | K::METHOD | K::CONSTRUCTOR) => ("ƒ", theme.syntax.function),
        Some(K::VARIABLE | K::FIELD | K::PROPERTY) => ("v", theme.syntax.variable),
        Some(K::CLASS | K::STRUCT | K::INTERFACE | K::ENUM | K::TYPE_PARAMETER) => {
            ("T", theme.syntax.r#type)
        }
        Some(K::KEYWORD) => ("k", theme.syntax.keyword),
        Some(K::MODULE) => ("m", theme.syntax.r#type),
        Some(K::CONSTANT | K::ENUM_MEMBER) => ("c", theme.syntax.number),
        Some(K::SNIPPET) => ("s", theme.accent),
        _ => ("·", theme.fg_subtle),
    }
}

/// Expands an LSP snippet to plain text. Returns the text and the range to
/// select afterwards: the first placeholder, or the final `$0` cursor, or the
/// end of the text.
pub fn expand_snippet(snippet: &str) -> (String, Range<usize>) {
    let mut out = String::with_capacity(snippet.len());
    // (tabstop number, range in `out`)
    let mut stops: Vec<(u32, Range<usize>)> = Vec::new();
    let chars: Vec<char> = snippet.chars().collect();
    let mut i = 0;
    fn parse_number(chars: &[char], i: &mut usize) -> Option<u32> {
        let start = *i;
        while *i < chars.len() && chars[*i].is_ascii_digit() {
            *i += 1;
        }
        chars[start..*i].iter().collect::<String>().parse().ok()
    }
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c != '$' {
            out.push(c);
            i += 1;
            continue;
        }
        // `$1`
        let mut j = i + 1;
        if let Some(n) = parse_number(&chars, &mut j) {
            stops.push((n, out.len()..out.len()));
            i = j;
            continue;
        }
        // `${1}`, `${1:default}`, `${1|a,b|}`
        if chars.get(i + 1) == Some(&'{') {
            let mut j = i + 2;
            if let Some(n) = parse_number(&chars, &mut j) {
                let start = out.len();
                match chars.get(j) {
                    Some(':') => {
                        j += 1;
                        let mut depth = 0;
                        while j < chars.len() {
                            match chars[j] {
                                '\\' if j + 1 < chars.len() => {
                                    out.push(chars[j + 1]);
                                    j += 1;
                                }
                                '{' => {
                                    depth += 1;
                                    out.push('{');
                                }
                                '}' if depth == 0 => break,
                                '}' => {
                                    depth -= 1;
                                    out.push('}');
                                }
                                // Nested tabstops inside a placeholder keep their default text only.
                                '$' => {}
                                ch => out.push(ch),
                            }
                            j += 1;
                        }
                    }
                    Some('|') => {
                        j += 1;
                        let mut first = String::new();
                        while j < chars.len() && chars[j] != ',' && chars[j] != '|' {
                            first.push(chars[j]);
                            j += 1;
                        }
                        out.push_str(&first);
                        while j < chars.len() && chars[j] != '}' {
                            j += 1;
                        }
                    }
                    _ => {}
                }
                stops.push((n, start..out.len()));
                i = j + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    let len = out.len();
    let selection = stops
        .iter()
        .filter(|(n, _)| *n > 0)
        .min_by_key(|(n, _)| *n)
        .or_else(|| stops.iter().find(|(n, _)| *n == 0))
        .map_or(len..len, |(_, r)| r.clone());
    (out, selection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_expand_to_text_and_selection() {
        assert_eq!(expand_snippet("println!($0)"), ("println!()".into(), 9..9));
        assert_eq!(
            expand_snippet("fn ${1:name}(${2:args}) {$0}"),
            ("fn name(args) {}".into(), 3..7)
        );
        assert_eq!(expand_snippet("a${1|x,y|}b"), ("axb".into(), 1..2));
        assert_eq!(expand_snippet("cost: \\$5"), ("cost: $5".into(), 8..8));
        assert_eq!(expand_snippet("plain"), ("plain".into(), 5..5));
    }

    #[test]
    fn menu_filters_by_typed_word() {
        let item = |label: &str| lt::CompletionItem {
            label: label.into(),
            ..Default::default()
        };
        let mut menu = CompletionMenu::new(
            vec![item("to_string"), item("trim"), item("to_owned")],
            Encoding::Utf8,
            0,
        );
        assert!(menu.filter("tos"));
        assert_eq!(menu.selected_item().unwrap().label, "to_string");
        assert!(!menu.filter("zzz"));
    }
}
