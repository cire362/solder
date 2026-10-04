//! The completion menu's state and snippet expansion. Requests and
//! keyboard handling live in the editor.

use std::{ops::Range, sync::Arc};

use extension::host::{CodeLabel, Completion, LabelSpan};
use gpui::{HighlightStyle, Hsla, SharedString, StyledText, UniformListScrollHandle};
use lsp::{Encoding, types as lt};
use syntax::HighlightKind;

use crate::{fuzzy, theme::Theme};

/// A completion's label as the extension that brought its server paints
/// it: the text to show, colored as code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub text: String,
    pub runs: Vec<(Range<usize>, HighlightKind)>,
    /// The part of the text the typed word is matched against.
    pub filter: Range<usize>,
}

impl Label {
    /// Puts together what an extension answered. `language` is the file's:
    /// the label's code is highlighted with its grammar, which parses it,
    /// so this is for a background thread.
    pub fn paint(label: &CodeLabel, language: Option<&Arc<syntax::Language>>) -> Label {
        let mut code: Option<Vec<(Range<usize>, HighlightKind)>> = None;
        let mut text = String::new();
        let mut runs = Vec::new();
        for span in &label.spans {
            match span {
                LabelSpan::Code(range) => {
                    let Some(part) = label.code.get(range.clone()) else {
                        continue;
                    };
                    let highlights = code.get_or_insert_with(|| {
                        language
                            .map(|language| syntax::highlight_code(language, &label.code))
                            .unwrap_or_default()
                    });
                    for (at, kind) in highlights.iter() {
                        let start = at.start.max(range.start);
                        let end = at.end.min(range.end);
                        if start < end {
                            let at = text.len() + start - range.start;
                            runs.push((at..at + end - start, *kind));
                        }
                    }
                    text.push_str(part);
                }
                LabelSpan::Literal {
                    text: literal,
                    highlight,
                } => {
                    let kind = highlight.as_deref().and_then(HighlightKind::from_capture);
                    if let Some(kind) = kind {
                        runs.push((text.len()..text.len() + literal.len(), kind));
                    }
                    text.push_str(literal);
                }
            }
        }
        // A menu row is one line.
        if text.contains('\n') {
            text = text.replace('\n', " ");
        }
        let filter = match text.get(label.filter.clone()) {
            Some(_) if !label.filter.is_empty() => label.filter.clone(),
            _ => 0..text.len(),
        };
        Label { text, runs, filter }
    }

    /// The text as an element: its code colors, and `accent` on the
    /// characters of the filter part that the typed word matched.
    pub fn styled(&self, positions: &[u32], theme: &Theme) -> StyledText {
        let mut colors: Vec<Option<Hsla>> = vec![None; self.text.len()];
        for (range, kind) in &self.runs {
            if let Some(slots) = colors.get_mut(range.clone()) {
                slots.fill(Some(theme.syntax.color(*kind)));
            }
        }
        let mut matched = positions.iter().peekable();
        for (i, (byte, c)) in self.text[self.filter.clone()].char_indices().enumerate() {
            if matched.peek().is_some_and(|p| **p as usize == i) {
                matched.next();
                let start = self.filter.start + byte;
                colors[start..start + c.len_utf8()].fill(Some(theme.accent));
            }
        }
        // One highlight for each stretch of one color, in order.
        let mut highlights = Vec::new();
        let mut start = 0;
        for end in 1..=colors.len() {
            if end == colors.len() || colors[end] != colors[start] {
                if let Some(color) = colors[start] {
                    highlights.push((
                        start..end,
                        HighlightStyle {
                            color: Some(color),
                            ..Default::default()
                        },
                    ));
                }
                start = end;
            }
        }
        StyledText::new(SharedString::from(self.text.clone())).with_highlights(highlights)
    }
}

/// What an extension is told about a completion when it is asked to paint
/// it.
pub fn for_extension(item: &lt::CompletionItem) -> Completion {
    Completion {
        label: item.label.clone(),
        detail: item.detail.clone(),
        label_detail: item.label_details.as_ref().and_then(|d| d.detail.clone()),
        label_description: item
            .label_details
            .as_ref()
            .and_then(|d| d.description.clone()),
        kind: item
            .kind
            .and_then(|kind| serde_json::to_value(kind).ok()?.as_i64())
            .map(|kind| kind as i32),
        format: item
            .insert_text_format
            .and_then(|format| serde_json::to_value(format).ok()?.as_i64())
            .map(|format| format as i32),
    }
}

pub struct CompletionMenu {
    pub items: Arc<Vec<lt::CompletionItem>>,
    /// For each item, how its extension paints it, if it does.
    pub labels: Vec<Option<Label>>,
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
        let items = items.into_iter().map(|item| (item, None)).collect();
        Self::painted(items, encoding, word_start)
    }

    /// A menu of items that may each come with a label an extension
    /// painted.
    pub fn painted(
        mut items: Vec<(lt::CompletionItem, Option<Label>)>,
        encoding: Encoding,
        word_start: usize,
    ) -> Self {
        // Server order is only a hint; sort_text is the contract.
        items.sort_by(|(a, _), (b, _)| {
            let ka = a.sort_text.as_deref().unwrap_or(&a.label);
            let kb = b.sort_text.as_deref().unwrap_or(&b.label);
            ka.cmp(kb)
        });
        let (items, labels) = items.into_iter().unzip();
        Self {
            items: Arc::new(items),
            labels,
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
                    // The matched characters are shown in what is drawn:
                    // the painted label's filter part, or the plain label.
                    let label = match &self.labels[m.index] {
                        Some(label) => &label.text[label.filter.clone()],
                        None => &self.items[m.index].label,
                    };
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

    #[test]
    fn a_label_is_put_together_from_what_an_extension_answered() {
        let js = syntax::language_for_path(std::path::Path::new("a.js")).unwrap();
        // Code that is parsed and shown in part, around text of its own.
        let label = CodeLabel {
            code: "function go(a) {}".into(),
            spans: vec![
                LabelSpan::Literal {
                    text: "fn ".into(),
                    highlight: Some("keyword".into()),
                },
                LabelSpan::Code(9..14),
                LabelSpan::Literal {
                    text: " -> void".into(),
                    highlight: None,
                },
                // Out of the code's range: left out, not a panic.
                LabelSpan::Code(40..50),
            ],
            filter: 3..5,
        };
        let painted = Label::paint(&label, Some(&js));
        assert_eq!(painted.text, "fn go(a) -> void");
        assert_eq!(painted.filter, 3..5);
        let kinds: Vec<(&str, HighlightKind)> = painted
            .runs
            .iter()
            .map(|(range, kind)| (&painted.text[range.clone()], *kind))
            .collect();
        assert_eq!(kinds[0], ("fn ", HighlightKind::Keyword));
        assert!(
            kinds.contains(&("go", HighlightKind::Function)),
            "{kinds:?}"
        );
        assert!(kinds.contains(&("a", HighlightKind::Variable)), "{kinds:?}");
        // A filter range that is not in the text means all of it.
        let odd = CodeLabel {
            code: String::new(),
            spans: vec![LabelSpan::Literal {
                text: "né".into(),
                highlight: Some("tag".into()),
            }],
            filter: 0..2,
        };
        let painted = Label::paint(&odd, None);
        assert_eq!(painted.filter, 0..3);
        assert_eq!(painted.runs, [(0..3, HighlightKind::Tag)]);

        // In the menu, the typed word is matched against the item and
        // shown in the part of the label that is its name.
        let item = lt::CompletionItem {
            label: "go".into(),
            ..Default::default()
        };
        let label = Label::paint(&label, Some(&js));
        let mut menu = CompletionMenu::painted(vec![(item, Some(label))], Encoding::Utf8, 0);
        assert!(menu.filter("g"));
        assert_eq!(menu.filtered, [(0, vec![0])]);

        let item = lt::CompletionItem {
            label: "div".into(),
            detail: Some("An element".into()),
            kind: Some(lt::CompletionItemKind::PROPERTY),
            insert_text_format: Some(lt::InsertTextFormat::SNIPPET),
            label_details: Some(lt::CompletionItemLabelDetails {
                detail: Some("(…)".into()),
                description: None,
            }),
            ..Default::default()
        };
        let told = for_extension(&item);
        assert_eq!((told.kind, told.format), (Some(10), Some(2)));
        assert_eq!(told.label_detail.as_deref(), Some("(…)"));
        assert_eq!(told.detail.as_deref(), Some("An element"));
    }
}
