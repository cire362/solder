//! Incremental parsing and highlighting on top of tree-sitter.
//!
//! Two rules keep this off the typing path:
//! - grammars and their highlight queries compile lazily, on the first file
//!   that needs them, so startup never pays for languages you do not open;
//! - highlights are queried only for the byte range on screen, so a keystroke
//!   in a 50k-line file costs the same as one in a 50-line file.

use std::{
    ops::Range,
    path::Path,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use text::{Edit, Rope};
use tree_sitter::{
    InputEdit, Node, ParseOptions, ParseState, Parser, Query, QueryCursor, StreamingIterator,
    TextProvider, Tree,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum HighlightKind {
    Keyword,
    String,
    Function,
    Type,
    Comment,
    Number,
    Constant,
    Property,
    Punctuation,
    Operator,
    Tag,
    Attribute,
    Variable,
}

impl HighlightKind {
    /// Maps a capture name such as `function.method` to a kind, trying the most
    /// specific names first.
    fn from_capture(name: &str) -> Option<Self> {
        use HighlightKind::*;
        Some(match name {
            "string.special.key" => Property,
            "variable.builtin" | "constant.builtin" => Constant,
            "type.builtin" | "constructor" => Type,
            "variable.parameter" | "variable" => Variable,
            "embedded" => return None,
            "charset" | "import" | "keyframes" | "media" | "namespace" | "supports" => Keyword,
            "escape" | "label" => Constant,
            _ => match name.split('.').next()? {
                "keyword" => Keyword,
                "string" => String,
                "function" => Function,
                "type" => Type,
                "comment" => Comment,
                "number" => Number,
                "constant" => Constant,
                "property" => Property,
                "punctuation" => Punctuation,
                "operator" => Operator,
                "tag" => Tag,
                "attribute" => Attribute,
                _ => return None,
            },
        })
    }
}

pub struct Language {
    pub name: &'static str,
    grammar: tree_sitter::Language,
    query_source: fn() -> String,
    compiled: OnceLock<Option<Highlighter>>,
}

struct Highlighter {
    query: Query,
    kinds: Vec<Option<HighlightKind>>,
}

impl Language {
    fn highlighter(&self) -> Option<&Highlighter> {
        self.compiled
            .get_or_init(|| {
                let query = Query::new(&self.grammar, &(self.query_source)())
                    .map_err(|e| eprintln!("highlight query for {} failed: {e}", self.name))
                    .ok()?;
                let kinds = query
                    .capture_names()
                    .iter()
                    .map(|n| HighlightKind::from_capture(n))
                    .collect();
                Some(Highlighter { query, kinds })
            })
            .as_ref()
    }
}

macro_rules! lang {
    ($name:literal, $grammar:expr, $($query:expr),+) => {
        Arc::new(Language {
            name: $name,
            grammar: $grammar.into(),
            query_source: || [$($query),+].join("\n"),
            compiled: OnceLock::new(),
        })
    };
}

fn registry() -> &'static [(&'static [&'static str], Arc<Language>)] {
    static REGISTRY: OnceLock<Vec<(&'static [&'static str], Arc<Language>)>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        // TypeScript's query only adds TS-specific captures on top of the
        // JavaScript one. Patterns listed first win, so TS goes first.
        vec![
            (
                &["rs"][..],
                lang!(
                    "Rust",
                    tree_sitter_rust::LANGUAGE,
                    tree_sitter_rust::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["ts", "mts", "cts"][..],
                lang!(
                    "TypeScript",
                    tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                    tree_sitter_javascript::HIGHLIGHT_QUERY
                ),
            ),
            (
                &["tsx"][..],
                lang!(
                    "TSX",
                    tree_sitter_typescript::LANGUAGE_TSX,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                    tree_sitter_javascript::HIGHLIGHT_QUERY
                ),
            ),
            (
                &["js", "mjs", "cjs", "jsx"][..],
                lang!(
                    "JavaScript",
                    tree_sitter_javascript::LANGUAGE,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                    tree_sitter_javascript::HIGHLIGHT_QUERY
                ),
            ),
            (
                &["json", "jsonc"][..],
                lang!(
                    "JSON",
                    tree_sitter_json::LANGUAGE,
                    tree_sitter_json::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["css"][..],
                lang!(
                    "CSS",
                    tree_sitter_css::LANGUAGE,
                    tree_sitter_css::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["go"][..],
                lang!(
                    "Go",
                    tree_sitter_go::LANGUAGE,
                    tree_sitter_go::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["py", "pyi"][..],
                lang!(
                    "Python",
                    tree_sitter_python::LANGUAGE,
                    tree_sitter_python::HIGHLIGHTS_QUERY
                ),
            ),
        ]
    })
}

pub fn language_for_path(path: &Path) -> Option<Arc<Language>> {
    let ext = path.extension()?.to_str()?;
    registry()
        .iter()
        .find(|(exts, _)| exts.contains(&ext))
        .map(|(_, l)| l.clone())
}

/// A parsed buffer. Cheap to move across threads, so the first parse of a large
/// file can run in the background while the text is already editable.
#[derive(Clone)]
pub struct SyntaxTree {
    language: Arc<Language>,
    tree: Tree,
    /// True when edits were applied to `tree` but it has not been reparsed yet.
    stale: bool,
}

impl SyntaxTree {
    pub fn parse(language: Arc<Language>, rope: &Rope) -> Option<Self> {
        let mut parser = new_parser(&language)?;
        let tree = parse_rope(&mut parser, rope, None, None)?;
        Some(Self {
            language,
            tree,
            stale: false,
        })
    }

    pub fn language(&self) -> &Arc<Language> {
        &self.language
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// Shifts the old tree to match an edit. Call once per applied edit, in
    /// the order the buffer reported them, then reparse.
    pub fn edit(&mut self, edit: &Edit) {
        self.tree.edit(&input_edit(edit));
        self.stale = true;
    }

    /// Incremental reparse that gives up after `budget`. Returns false when it
    /// ran out of time; the tree keeps its edited-but-stale state and the
    /// caller should finish with [`SyntaxTree::reparse_in_background`].
    pub fn reparse_within(&mut self, rope: &Rope, budget: Duration) -> bool {
        let Some(mut parser) = new_parser(&self.language) else {
            return false;
        };
        let deadline = Instant::now() + budget;
        match parse_rope(&mut parser, rope, Some(&self.tree), Some(deadline)) {
            Some(tree) => {
                self.tree = tree;
                self.stale = false;
                true
            }
            None => false,
        }
    }

    /// Full-length incremental reparse against a rope snapshot. Meant to run on
    /// a worker thread; the result replaces the tree only if the buffer has not
    /// changed since the snapshot was taken.
    pub fn reparse_in_background(&self, rope: &Rope) -> Option<SyntaxTree> {
        let mut parser = new_parser(&self.language)?;
        let tree = parse_rope(&mut parser, rope, Some(&self.tree), None)?;
        Some(SyntaxTree {
            language: self.language.clone(),
            tree,
            stale: false,
        })
    }

    /// Non-overlapping highlight spans intersecting `range`, sorted by start.
    /// Nested captures override their parents (an escape inside a string);
    /// for the same node, the first matching pattern wins.
    pub fn highlights(
        &self,
        rope: &Rope,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, HighlightKind)> {
        let Some(hl) = self.language.highlighter() else {
            return Vec::new();
        };
        let range = range.start.min(rope.len_bytes())..range.end.min(rope.len_bytes());
        if range.is_empty() {
            return Vec::new();
        }

        // One slot per byte of the visible range. A screen of code is a few KB,
        // so this is cheaper than interval bookkeeping.
        const NONE: u8 = u8::MAX;
        let mut slots = vec![NONE; range.len()];
        let mut painted: Vec<Range<usize>> = Vec::new();

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let mut captures = cursor.captures(&hl.query, self.tree.root_node(), RopeProvider(rope));
        while let Some((m, index)) = captures.next() {
            let capture = m.captures[*index];
            let Some(kind) = hl.kinds[capture.index as usize] else {
                continue;
            };
            let node = capture.node.byte_range();
            if painted.last().is_some_and(|r| *r == node) || painted.contains(&node) {
                continue;
            }
            painted.push(node.clone());
            let start = node.start.max(range.start) - range.start;
            let end = node.end.min(range.end).saturating_sub(range.start);
            if start < end {
                slots[start..end].fill(kind as u8);
            }
        }

        let mut spans = Vec::new();
        let mut i = 0;
        while i < slots.len() {
            let k = slots[i];
            let start = i;
            while i < slots.len() && slots[i] == k {
                i += 1;
            }
            if k != NONE {
                // SAFETY: every non-NONE slot was written from a HighlightKind.
                let kind: HighlightKind = unsafe { std::mem::transmute(k) };
                spans.push((range.start + start..range.start + i, kind));
            }
        }
        spans
    }
}

fn new_parser(language: &Language) -> Option<Parser> {
    let mut parser = Parser::new();
    parser.set_language(&language.grammar).ok()?;
    Some(parser)
}

fn parse_rope(
    parser: &mut Parser,
    rope: &Rope,
    old: Option<&Tree>,
    deadline: Option<Instant>,
) -> Option<Tree> {
    let len = rope.len_bytes();
    let mut read = |byte: usize, _| -> &[u8] {
        if byte >= len {
            return &[];
        }
        let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - chunk_start..]
    };
    match deadline {
        None => parser.parse_with_options(&mut read, old, None),
        Some(deadline) => {
            // Returning true from the progress callback cancels the parse.
            let mut over_budget = |_: &ParseState| Instant::now() >= deadline;
            let options = ParseOptions::new().progress_callback(&mut over_budget);
            let tree = parser.parse_with_options(&mut read, old, Some(options));
            if tree.is_none() {
                parser.reset();
            }
            tree
        }
    }
}

fn input_edit(e: &Edit) -> InputEdit {
    let p = |p: text::Point| tree_sitter::Point::new(p.row, p.column);
    InputEdit {
        start_byte: e.start,
        old_end_byte: e.old_end,
        new_end_byte: e.new_end,
        start_position: p(e.start_point),
        old_end_position: p(e.old_end_point),
        new_end_position: p(e.new_end_point),
    }
}

struct RopeProvider<'a>(&'a Rope);

type ChunkBytes<'a> = std::iter::Map<ropey::iter::Chunks<'a>, fn(&'a str) -> &'a [u8]>;

impl<'a> TextProvider<&'a [u8]> for RopeProvider<'a> {
    type I = ChunkBytes<'a>;

    fn text(&mut self, node: Node) -> Self::I {
        self.0
            .byte_slice(node.byte_range())
            .chunks()
            .map(str::as_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use text::Buffer;

    fn kinds(tree: &SyntaxTree, rope: &Rope, src: &str) -> Vec<(String, HighlightKind)> {
        tree.highlights(rope, 0..rope.len_bytes())
            .into_iter()
            .map(|(r, k)| (src[r].to_string(), k))
            .collect()
    }

    #[test]
    fn every_language_query_compiles() {
        for (_, lang) in registry() {
            assert!(lang.highlighter().is_some(), "{} query failed", lang.name);
        }
    }

    #[test]
    fn highlights_rust() {
        let src = "fn main() { let x = \"hi\"; // note\n}";
        let rope = Rope::from_str(src);
        let lang = language_for_path(Path::new("a.rs")).unwrap();
        let tree = SyntaxTree::parse(lang, &rope).unwrap();
        let spans = kinds(&tree, &rope, src);
        assert!(spans.contains(&("fn".into(), HighlightKind::Keyword)));
        assert!(spans.contains(&("main".into(), HighlightKind::Function)));
        assert!(spans.contains(&("\"hi\"".into(), HighlightKind::String)));
        assert!(spans.contains(&("// note".into(), HighlightKind::Comment)));
    }

    #[test]
    fn highlights_only_requested_range() {
        let src = "const a = 1;\nconst b = 2;\n";
        let rope = Rope::from_str(src);
        let lang = language_for_path(Path::new("a.ts")).unwrap();
        let tree = SyntaxTree::parse(lang, &rope).unwrap();
        let spans = tree.highlights(&rope, 13..26);
        assert!(spans.iter().all(|(r, _)| r.start >= 13 && r.end <= 26));
        assert!(
            spans
                .iter()
                .any(|(r, k)| &src[r.clone()] == "const" && *k == HighlightKind::Keyword)
        );
    }

    #[test]
    fn incremental_reparse_tracks_edits() {
        let mut buffer = Buffer::new("fn a() {}\n");
        let lang = language_for_path(Path::new("a.rs")).unwrap();
        let mut tree = SyntaxTree::parse(lang, buffer.rope()).unwrap();
        for e in buffer.edit([(10..10, "fn b() { \"s\" }\n")], &[], Instant::now()) {
            tree.edit(&e);
        }
        assert!(tree.is_stale());
        assert!(tree.reparse_within(buffer.rope(), Duration::from_millis(50)));
        let src = buffer.rope().to_string();
        let spans = kinds(&tree, buffer.rope(), &src);
        assert!(spans.contains(&("b".into(), HighlightKind::Function)));
        assert!(spans.contains(&("\"s\"".into(), HighlightKind::String)));
    }

    #[test]
    fn parses_across_rope_chunks() {
        // Large enough that the rope splits it into many chunks.
        let src: String = (0..2000).map(|i| format!("let v{i} = {i};\n")).collect();
        let rope = Rope::from_str(&src);
        assert!(rope.chunks().count() > 1);
        let lang = language_for_path(Path::new("a.js")).unwrap();
        let tree = SyntaxTree::parse(lang, &rope).unwrap();
        assert!(!tree.tree.root_node().has_error());
    }
}
