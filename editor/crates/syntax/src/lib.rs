//! Incremental parsing and highlighting on top of tree-sitter.
//!
//! Two rules keep this off the typing path:
//! - grammars and their highlight queries compile lazily, on the first file
//!   that needs them, so startup never pays for languages you do not open;
//! - highlights are queried only for the byte range on screen, so a keystroke
//!   in a 50k-line file costs the same as one in a 50-line file.

use std::{
    ops::{ControlFlow, Range},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::{Duration, Instant},
};

use text::{Edit, Rope};
use tree_sitter::{
    InputEdit, Node, ParseOptions, ParseState, Parser, Query, QueryCursor, StreamingIterator,
    TextProvider, Tree,
};

mod outline;
mod rules;
mod wasm;
pub use outline::{Symbol, outline};
pub use rules::{Editing, Indent, Line, Pair};

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
    pub fn from_capture(name: &str) -> Option<Self> {
        use HighlightKind::*;
        Some(match name {
            "string.special.key" => Property,
            "variable.builtin" | "constant.builtin" => Constant,
            "type.builtin" | "constructor" => Type,
            "variable.parameter" | "variable" => Variable,
            "embedded" | "emphasis" | "link_text" => return None,
            "boolean" | "variant" => Constant,
            "enum" => Type,
            "preproc" | "title" => Keyword,
            "text" | "link_uri" => String,
            "selector" => Tag,
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

/// A language installed with an extension: a tree-sitter grammar compiled to
/// WebAssembly and the query files next to it. Nothing is read until a file
/// of the language is opened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LanguageSpec {
    pub name: String,
    /// File name endings without the dot (`vue`) and whole names (`Dockerfile`).
    pub suffixes: Vec<String>,
    /// Other names an injection may call it by, such as a code fence name.
    pub aliases: Vec<String>,
    pub line_comment: Option<String>,
    /// The grammar's name inside the module (`tree_sitter_<symbol>`).
    pub symbol: String,
    pub grammar: PathBuf,
    pub highlights: Option<PathBuf>,
    pub injections: Option<PathBuf>,
    pub editing: Editing,
}

enum Source {
    Native {
        grammar: tree_sitter::Language,
        query: fn() -> String,
    },
    Wasm(Box<LanguageSpec>),
}

pub struct Language {
    pub name: &'static str,
    /// What starts a comment that runs to the end of the line.
    pub line_comment: Option<&'static str>,
    source: Source,
    grammar: OnceLock<Option<tree_sitter::Language>>,
    highlighter: OnceLock<Option<Highlighter>>,
    injector: OnceLock<Option<Injector>>,
    rules: OnceLock<rules::Rules>,
}

struct Highlighter {
    query: Query,
    kinds: Vec<Option<HighlightKind>>,
}

/// The query that finds other languages inside this one, such as the script
/// of a Vue file.
struct Injector {
    query: Query,
    content: Option<u32>,
    language: Option<u32>,
    patterns: Vec<Injection>,
}

#[derive(Default)]
struct Injection {
    language: Option<String>,
    /// All matches of the pattern form one document.
    combined: bool,
    include_children: bool,
}

impl Language {
    /// False while the grammar still has to be compiled, which takes long
    /// enough to stay off the UI thread.
    pub fn is_ready(&self) -> bool {
        self.grammar.get().is_some()
    }

    fn grammar(&self) -> Option<&tree_sitter::Language> {
        self.grammar
            .get_or_init(|| match &self.source {
                Source::Native { grammar, .. } => Some(grammar.clone()),
                Source::Wasm(spec) => wasm::load(spec)
                    .map_err(|e| eprintln!("grammar for {} failed: {e}", self.name))
                    .ok(),
            })
            .as_ref()
    }

    fn query(&self, source: &str, what: &str) -> Option<Query> {
        Query::new(self.grammar()?, source)
            .map_err(|e| eprintln!("{what} query for {} failed: {e}", self.name))
            .ok()
    }

    fn highlighter(&self) -> Option<&Highlighter> {
        self.highlighter
            .get_or_init(|| {
                let source = match &self.source {
                    Source::Native { query, .. } => query(),
                    Source::Wasm(spec) => {
                        std::fs::read_to_string(spec.highlights.as_ref()?).ok()?
                    }
                };
                let query = self.query(&source, "highlight")?;
                let kinds = query
                    .capture_names()
                    .iter()
                    .map(|n| HighlightKind::from_capture(n))
                    .collect();
                Some(Highlighter { query, kinds })
            })
            .as_ref()
    }

    fn injector(&self) -> Option<&Injector> {
        self.injector
            .get_or_init(|| {
                let Source::Wasm(spec) = &self.source else {
                    return None;
                };
                let source = std::fs::read_to_string(spec.injections.as_ref()?).ok()?;
                let query = self.query(&source, "injection")?;
                // Both spellings are in use: `injection.content` in current
                // grammars, plain `content` in older ones.
                let capture =
                    |names: [&str; 2]| names.iter().find_map(|n| query.capture_index_for_name(n));
                let patterns = (0..query.pattern_count())
                    .map(|pattern| {
                        let mut injection = Injection::default();
                        for property in query.property_settings(pattern) {
                            match &*property.key {
                                "injection.language" | "language" => {
                                    injection.language =
                                        property.value.as_deref().map(str::to_lowercase)
                                }
                                "injection.combined" | "combined" => injection.combined = true,
                                "injection.include-children" => injection.include_children = true,
                                _ => {}
                            }
                        }
                        injection
                    })
                    .collect();
                Some(Injector {
                    content: capture(["injection.content", "content"]),
                    language: capture(["injection.language", "language"]),
                    query,
                    patterns,
                })
            })
            .as_ref()
    }

    fn rules(&self) -> &rules::Rules {
        self.rules.get_or_init(|| rules::Rules::load(self))
    }

    /// What the extension that brought the language says about typing in
    /// it. A built-in language has none: the editor's own rules hold there.
    pub fn editing(&self) -> Option<&Editing> {
        match &self.source {
            Source::Native { .. } => None,
            Source::Wasm(spec) => Some(&spec.editing),
        }
    }

    /// Whether [`outline`] has more than a guess to go by for this language.
    pub fn has_outline(&self) -> bool {
        self.editing().is_some_and(|e| e.outline.is_some())
    }

    /// The lowercase names this language goes by elsewhere: what snippet
    /// files and VS Code call it, and for an extension's language its
    /// aliases and file endings.
    pub fn ids(&self) -> Vec<String> {
        let mut ids = vec![self.name.to_lowercase()];
        match &self.source {
            Source::Native { .. } => ids.extend(
                match self.name {
                    "TSX" => &["typescriptreact"][..],
                    "JavaScript" => &["javascriptreact", "jsx"][..],
                    "JSON" => &["jsonc"][..],
                    _ => &[][..],
                }
                .iter()
                .map(|id| id.to_string()),
            ),
            Source::Wasm(spec) => ids.extend(
                spec.aliases
                    .iter()
                    .chain(&spec.suffixes)
                    .map(|id| id.to_lowercase()),
            ),
        }
        ids.dedup();
        ids
    }

    /// Whether an injection that asks for `name` (lowercase) means this
    /// language.
    fn answers_to(&self, name: &str) -> bool {
        if self.name.eq_ignore_ascii_case(name) {
            return true;
        }
        match &self.source {
            Source::Native { .. } => builtin_alias(name) == Some(self.name),
            Source::Wasm(spec) => spec
                .aliases
                .iter()
                .chain(&spec.suffixes)
                .any(|a| a.eq_ignore_ascii_case(name)),
        }
    }
}

/// The short names injections use for the built-in languages.
fn builtin_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "rs" => "Rust",
        "ts" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "golang" => "Go",
        "jsonc" => "JSON",
        _ => return None,
    })
}

/// Language names live as long as the program: the status bar and the
/// language server table compare them as `&'static str`. Extension languages
/// are few, so each distinct name is kept once and never freed.
fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap();
    if let Some(known) = names.iter().find(|n| **n == name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.push(leaked);
    leaked
}

macro_rules! lang {
    ($name:literal, $comment:literal, $grammar:expr, $($query:expr),+) => {
        Arc::new(Language {
            name: $name,
            line_comment: (!$comment.is_empty()).then_some($comment),
            source: Source::Native {
                grammar: $grammar.into(),
                query: || [$($query),+].join("\n"),
            },
            grammar: OnceLock::new(),
            highlighter: OnceLock::new(),
            injector: OnceLock::new(),
            rules: OnceLock::new(),
        })
    };
}

fn registry() -> &'static [(&'static [&'static str], Arc<Language>)] {
    static REGISTRY: OnceLock<Vec<(&'static [&'static str], Arc<Language>)>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        // TypeScript's query only adds TS-specific captures on top of the
        // JavaScript one. Later patterns win, so TS goes last.
        vec![
            (
                &["rs"][..],
                lang!(
                    "Rust",
                    "//",
                    tree_sitter_rust::LANGUAGE,
                    tree_sitter_rust::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["ts", "mts", "cts"][..],
                lang!(
                    "TypeScript",
                    "//",
                    tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["tsx"][..],
                lang!(
                    "TSX",
                    "//",
                    tree_sitter_typescript::LANGUAGE_TSX,
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["js", "mjs", "cjs", "jsx"][..],
                lang!(
                    "JavaScript",
                    "//",
                    tree_sitter_javascript::LANGUAGE,
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
                ),
            ),
            (
                &["json", "jsonc"][..],
                lang!(
                    "JSON",
                    "//",
                    tree_sitter_json::LANGUAGE,
                    tree_sitter_json::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["css"][..],
                lang!(
                    "CSS",
                    "",
                    tree_sitter_css::LANGUAGE,
                    tree_sitter_css::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["go"][..],
                lang!(
                    "Go",
                    "//",
                    tree_sitter_go::LANGUAGE,
                    tree_sitter_go::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["py", "pyi"][..],
                lang!(
                    "Python",
                    "#",
                    tree_sitter_python::LANGUAGE,
                    tree_sitter_python::HIGHLIGHTS_QUERY
                ),
            ),
        ]
    })
}

/// Languages that came with extensions, in the order they were given.
fn extensions() -> &'static RwLock<Vec<Arc<Language>>> {
    static EXTENSIONS: RwLock<Vec<Arc<Language>>> = RwLock::new(Vec::new());
    &EXTENSIONS
}

/// Replaces the languages that come from extensions. A language whose files
/// did not change keeps its compiled grammar.
pub fn set_extension_languages(specs: Vec<LanguageSpec>) {
    let mut languages = extensions().write().unwrap();
    let old = std::mem::take(&mut *languages);
    *languages = specs
        .into_iter()
        .map(|spec| {
            let kept = old
                .iter()
                .find(|l| matches!(&l.source, Source::Wasm(known) if **known == spec));
            match kept {
                Some(language) => language.clone(),
                None => Arc::new(Language {
                    name: intern(&spec.name),
                    line_comment: spec.line_comment.as_deref().map(intern),
                    source: Source::Wasm(Box::new(spec)),
                    grammar: OnceLock::new(),
                    highlighter: OnceLock::new(),
                    injector: OnceLock::new(),
                    rules: OnceLock::new(),
                }),
            }
        })
        .collect();
}

pub fn language_for_path(path: &Path) -> Option<Arc<Language>> {
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && let Some((_, language)) = registry().iter().find(|(exts, _)| exts.contains(&ext))
    {
        return Some(language.clone());
    }
    // An extension's suffix is the whole file name (`Dockerfile`) or what
    // follows a dot (`vue`, `blade.php`).
    let name = path.file_name()?.to_str()?;
    extensions()
        .read()
        .unwrap()
        .iter()
        .find(|language| {
            let Source::Wasm(spec) = &language.source else {
                return false;
            };
            spec.suffixes.iter().any(|suffix| {
                name == suffix
                    || name
                        .strip_suffix(suffix.as_str())
                        .is_some_and(|rest| rest.ends_with('.'))
            })
        })
        .cloned()
}

/// The language an injection calls `name`: built-in ones first, then those
/// of extensions.
fn language_named(name: &str) -> Option<Arc<Language>> {
    let name = name.trim().to_lowercase();
    registry()
        .iter()
        .map(|(_, language)| language)
        .find(|language| language.answers_to(&name))
        .cloned()
        .or_else(|| {
            extensions()
                .read()
                .unwrap()
                .iter()
                .find(|language| language.answers_to(&name))
                .cloned()
        })
}

/// The highlights of a piece of code on its own, such as the label of a
/// completion. It is parsed here, which for an extension's language is
/// more than a keystroke should wait for: call off the UI thread.
pub fn highlight_code(language: &Arc<Language>, code: &str) -> Vec<(Range<usize>, HighlightKind)> {
    let rope = Rope::from_str(code);
    SyntaxTree::parse(language.clone(), &rope)
        .map(|tree| tree.highlights(&rope, 0..rope.len_bytes()))
        .unwrap_or_default()
}

/// A parsed buffer. Cheap to move across threads, so the first parse of a large
/// file can run in the background while the text is already editable.
#[derive(Clone)]
pub struct SyntaxTree {
    language: Arc<Language>,
    tree: Tree,
    /// Other languages found inside this one, each parsed over its own
    /// ranges of the same text. A layer comes after the one it was found in.
    layers: Vec<Layer>,
    /// True when edits were applied to `tree` but it has not been reparsed yet.
    stale: bool,
}

#[derive(Clone)]
struct Layer {
    language: Arc<Language>,
    tree: Tree,
    start: usize,
    end: usize,
}

/// How deep injections may nest: a template in a script in a page.
const MAX_DEPTH: usize = 3;

impl SyntaxTree {
    pub fn parse(language: Arc<Language>, rope: &Rope) -> Option<Self> {
        let tree = parse_rope(&mut *new_parser(&language)?, rope, None, None)?;
        // An extension's queries are read from disk and compiled here, where
        // the caller already expects to wait, and not at the first paint.
        if matches!(language.source, Source::Wasm(_)) {
            language.highlighter();
            language.rules();
        }
        let layers = parse_layers(&language, &tree, rope, &[], None)?;
        Some(Self {
            language,
            tree,
            layers,
            stale: false,
        })
    }

    pub fn language(&self) -> &Arc<Language> {
        &self.language
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// The language the byte at `offset` is written in: the innermost
    /// injected one that covers it, or the file's own.
    pub fn language_at(&self, offset: usize) -> &Arc<Language> {
        self.tree_at(offset).0
    }

    fn tree_at(&self, offset: usize) -> (&Arc<Language>, &Tree) {
        self.trees_at(offset)
            .next()
            .expect("the file's own tree is always there")
    }

    /// The trees that cover `offset`, the innermost first and the file's
    /// own last.
    fn trees_at(&self, offset: usize) -> impl Iterator<Item = (&Arc<Language>, &Tree)> {
        self.layers
            .iter()
            .rev()
            .filter(move |layer| {
                layer.start <= offset
                    && offset <= layer.end
                    && layer
                        .tree
                        .included_ranges()
                        .iter()
                        .any(|r| r.start_byte <= offset && offset <= r.end_byte)
            })
            .map(|layer| (&layer.language, &layer.tree))
            .chain(std::iter::once((&self.language, &self.tree)))
    }

    /// Shifts the old tree to match an edit. Call once per applied edit, in
    /// the order the buffer reported them, then reparse.
    pub fn edit(&mut self, edit: &Edit) {
        let edit = input_edit(edit);
        self.tree.edit(&edit);
        for layer in &mut self.layers {
            layer.tree.edit(&edit);
        }
        self.stale = true;
    }

    /// Incremental reparse that gives up after `budget`. Returns false when it
    /// ran out of time; the tree keeps its edited-but-stale state and the
    /// caller should finish with [`SyntaxTree::reparse_in_background`].
    pub fn reparse_within(&mut self, rope: &Rope, budget: Duration) -> bool {
        match self.reparsed(rope, Some(Instant::now() + budget)) {
            Some(fresh) => {
                *self = fresh;
                true
            }
            None => false,
        }
    }

    /// Full-length incremental reparse against a rope snapshot. Meant to run on
    /// a worker thread; the result replaces the tree only if the buffer has not
    /// changed since the snapshot was taken.
    pub fn reparse_in_background(&self, rope: &Rope) -> Option<SyntaxTree> {
        self.reparsed(rope, None)
    }

    fn reparsed(&self, rope: &Rope, deadline: Option<Instant>) -> Option<SyntaxTree> {
        let mut parser = new_parser(&self.language)?;
        let tree = parse_rope(&mut parser, rope, Some(&self.tree), deadline)?;
        drop(parser);
        let layers = parse_layers(&self.language, &tree, rope, &self.layers, deadline)?;
        Some(SyntaxTree {
            language: self.language.clone(),
            tree,
            layers,
            stale: false,
        })
    }

    /// Non-overlapping highlight spans intersecting `range`, sorted by start.
    /// Nested captures override their parents (an escape inside a string);
    /// for the same node, the last matching pattern wins, which is the order
    /// highlight queries are written for: the general rule first, then its
    /// exceptions. An injected language paints over the language it sits in.
    pub fn highlights(
        &self,
        rope: &Rope,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, HighlightKind)> {
        let range = range.start.min(rope.len_bytes())..range.end.min(rope.len_bytes());
        if range.is_empty() {
            return Vec::new();
        }

        // One slot per byte of the visible range. A screen of code is a few KB,
        // so this is cheaper than interval bookkeeping.
        let mut slots = vec![NONE; range.len()];
        paint(&self.language, &self.tree, rope, &range, &mut slots);
        for layer in &self.layers {
            if layer.start < range.end && layer.end > range.start {
                paint(&layer.language, &layer.tree, rope, &range, &mut slots);
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

const NONE: u8 = u8::MAX;

/// Writes the highlight of every capture of `tree` inside `range` into
/// `slots`, one per byte.
fn paint(language: &Language, tree: &Tree, rope: &Rope, range: &Range<usize>, slots: &mut [u8]) {
    let Some(hl) = language.highlighter() else {
        return;
    };
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range.clone());
    let mut captures = cursor.captures(&hl.query, tree.root_node(), RopeProvider(rope));
    while let Some((m, index)) = captures.next() {
        let capture = m.captures[*index];
        let Some(kind) = hl.kinds[capture.index as usize] else {
            continue;
        };
        // Captures come in document order, and for one node in the order
        // of their patterns, so writing each over the last gives the rule.
        let node = capture.node.byte_range();
        let start = node.start.max(range.start) - range.start;
        let end = node.end.min(range.end).saturating_sub(range.start);
        if start < end {
            slots[start..end].fill(kind as u8);
        }
    }
}

/// Finds the languages injected into `tree` and parses each over its ranges,
/// reusing the trees in `old` (already shifted by the edits). `None` means the
/// deadline passed.
fn parse_layers(
    language: &Arc<Language>,
    tree: &Tree,
    rope: &Rope,
    old: &[Layer],
    deadline: Option<Instant>,
) -> Option<Vec<Layer>> {
    let mut layers = Vec::new();
    if language.injector().is_some() {
        let mut used = vec![false; old.len()];
        inject(
            language,
            tree,
            rope,
            old,
            &mut used,
            deadline,
            1,
            &mut layers,
        )?;
    }
    Some(layers)
}

#[allow(clippy::too_many_arguments)] // one recursive walk; a struct would only rename them
fn inject(
    language: &Arc<Language>,
    tree: &Tree,
    rope: &Rope,
    old: &[Layer],
    used: &mut [bool],
    deadline: Option<Instant>,
    depth: usize,
    layers: &mut Vec<Layer>,
) -> Option<()> {
    let Some(injector) = language.injector() else {
        return Some(());
    };
    let Some(content) = injector.content else {
        return Some(());
    };

    // What to parse: a language and the ranges of the text it covers.
    let mut found: Vec<(Arc<Language>, Vec<tree_sitter::Range>)> = Vec::new();
    let mut combined: Vec<(usize, usize)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&injector.query, tree.root_node(), RopeProvider(rope));
    while let Some(m) = matches.next() {
        let pattern = &injector.patterns[m.pattern_index];
        let named = pattern.language.clone().or_else(|| {
            let node = m.nodes_for_capture_index(injector.language?).next()?;
            Some(rope.byte_slice(node.byte_range()).to_string())
        });
        let Some(target) = named.and_then(|name| language_named(&name)) else {
            continue;
        };
        let mut ranges = Vec::new();
        for node in m.nodes_for_capture_index(content) {
            content_ranges(node, pattern.include_children, &mut ranges);
        }
        if ranges.is_empty() {
            continue;
        }
        let slot = combined
            .iter()
            .find(|(p, _)| pattern.combined && *p == m.pattern_index)
            .map(|(_, slot)| *slot);
        match slot {
            Some(slot) => found[slot].1.extend(ranges),
            None => {
                if pattern.combined {
                    combined.push((m.pattern_index, found.len()));
                }
                found.push((target, ranges));
            }
        }
    }
    drop(matches);

    for (_, ranges) in &mut found {
        ranges.sort_by_key(|r| r.start_byte);
        // The parser refuses ranges that overlap.
        ranges.dedup_by(|next, kept| next.start_byte < kept.end_byte);
    }
    found.sort_by_key(|(_, ranges)| ranges[0].start_byte);

    for (target, ranges) in found {
        let Some(mut parser) = new_parser(&target) else {
            continue;
        };
        if parser.set_included_ranges(&ranges).is_err() {
            continue;
        }
        // The first unused old tree of the same language: layers keep their
        // order while typing, so this is the same piece of text.
        let reuse = old
            .iter()
            .enumerate()
            .find(|(i, layer)| !used[*i] && Arc::ptr_eq(&layer.language, &target))
            .map(|(i, layer)| {
                used[i] = true;
                &layer.tree
            });
        let parsed = parse_rope(&mut parser, rope, reuse, deadline)?;
        drop(parser);
        // Compiled here for the same reason as in `SyntaxTree::parse`.
        target.highlighter();
        target.rules();
        let layer = Layer {
            language: target,
            tree: parsed,
            start: ranges[0].start_byte,
            end: ranges[ranges.len() - 1].end_byte,
        };
        layers.push(layer.clone());
        if depth < MAX_DEPTH {
            inject(
                &layer.language,
                &layer.tree,
                rope,
                old,
                used,
                deadline,
                depth + 1,
                layers,
            )?;
        }
    }
    Some(())
}

/// The ranges of `node` an injected language reads: all of it, or what is
/// left between its children.
fn content_ranges(node: Node, include_children: bool, out: &mut Vec<tree_sitter::Range>) {
    let mut push = |start_byte, start_point, end_byte, end_point| {
        if start_byte < end_byte {
            out.push(tree_sitter::Range {
                start_byte,
                end_byte,
                start_point,
                end_point,
            });
        }
    };
    if include_children || node.child_count() == 0 {
        push(
            node.start_byte(),
            node.start_position(),
            node.end_byte(),
            node.end_position(),
        );
        return;
    }
    let (mut byte, mut point) = (node.start_byte(), node.start_position());
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        push(byte, point, child.start_byte(), child.start_position());
        (byte, point) = (child.end_byte(), child.end_position());
    }
    push(byte, point, node.end_byte(), node.end_position());
}

/// A parser set up for one language. One that runs a WebAssembly grammar
/// carries a store that is costly to create, so it returns to a pool.
struct LanguageParser {
    parser: Option<Parser>,
    pooled: bool,
}

impl std::ops::Deref for LanguageParser {
    type Target = Parser;

    fn deref(&self) -> &Parser {
        self.parser.as_ref().expect("taken only on drop")
    }
}

impl std::ops::DerefMut for LanguageParser {
    fn deref_mut(&mut self) -> &mut Parser {
        self.parser.as_mut().expect("taken only on drop")
    }
}

impl Drop for LanguageParser {
    fn drop(&mut self) {
        if let (true, Some(parser)) = (self.pooled, self.parser.take()) {
            wasm::give_back(parser);
        }
    }
}

fn new_parser(language: &Language) -> Option<LanguageParser> {
    let grammar = language.grammar()?;
    let pooled = grammar.is_wasm();
    let mut parser = if pooled {
        wasm::parser()?
    } else {
        Parser::new()
    };
    parser.set_language(grammar).ok()?;
    Some(LanguageParser {
        parser: Some(parser),
        pooled,
    })
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
            // Breaking out of the progress callback cancels the parse.
            let mut over_budget = |_: &ParseState| {
                if Instant::now() >= deadline {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
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
    fn a_later_pattern_wins_over_an_earlier_one() {
        // Queries name every identifier a variable first and say what else
        // it can be afterwards.
        let spans = |path: &str, src: &str| {
            let rope = Rope::from_str(src);
            let lang = language_for_path(Path::new(path)).unwrap();
            kinds(&SyntaxTree::parse(lang, &rope).unwrap(), &rope, src)
        };
        let ts = spans("a.ts", "function twice(n: number) { return go(n) }");
        assert!(ts.contains(&("twice".into(), HighlightKind::Function)));
        assert!(ts.contains(&("go".into(), HighlightKind::Function)));
        assert!(ts.contains(&("n".into(), HighlightKind::Variable)));
        assert!(ts.contains(&("number".into(), HighlightKind::Type)));
        let tsx = spans("a.tsx", "const a = <Box size={n}>x</Box>;");
        assert!(tsx.contains(&("size".into(), HighlightKind::Attribute)));
        let rust = spans("a.rs", "fn f(s: S) { s.len(); s.field; }");
        assert!(rust.contains(&("len".into(), HighlightKind::Function)));
        assert!(rust.contains(&("field".into(), HighlightKind::Property)));
        let py = spans("a.py", "def f(x):\n    return g(x)\n");
        assert!(py.contains(&("f".into(), HighlightKind::Function)));
        assert!(py.contains(&("g".into(), HighlightKind::Function)));
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

    /// The Vue language of the fixtures, registered the way an installed
    /// extension registers it. The registry is one per process, so every test
    /// that needs extension languages asks for the same set.
    fn vue() -> Arc<Language> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vue");
        set_extension_languages(vec![
            LanguageSpec {
                name: "Vue.js".into(),
                suffixes: vec!["vue".into()],
                aliases: vec!["vue".into()],
                line_comment: None,
                symbol: "vue".into(),
                grammar: dir.join("vue.wasm"),
                highlights: Some(dir.join("highlights.scm")),
                injections: Some(dir.join("injections.scm")),
                editing: Editing {
                    indents: Some(dir.join("indents.scm")),
                    brackets: Some(dir.join("brackets.scm")),
                    outline: Some(dir.join("outline.scm")),
                    overrides: Some(dir.join("overrides.scm")),
                    ..Default::default()
                },
            },
            // The same grammar under a name only one test opens files of, so
            // that test sees it before anything has compiled it.
            LanguageSpec {
                name: "Untouched".into(),
                suffixes: vec!["untouched".into()],
                symbol: "vue".into(),
                grammar: dir.join("vue.wasm"),
                ..Default::default()
            },
            LanguageSpec {
                name: "Dockerfile".into(),
                suffixes: vec!["Dockerfile".into(), "dockerfile".into()],
                grammar: dir.join("missing.wasm"),
                ..Default::default()
            },
        ]);
        language_for_path(Path::new("App.vue")).unwrap()
    }

    const COMPONENT: &str = "<template>\n  <div class=\"box\" @click=\"go(1)\">{{ msg }}</div>\n</template>\n<script setup lang=\"ts\">\nconst msg: string = 'hi'\n</script>\n<style>\n.box { color: red; }\n</style>\n";

    #[test]
    fn an_extension_language_is_found_by_suffix_or_whole_name() {
        let vue = vue();
        assert_eq!(vue.name, "Vue.js");
        // Nothing is compiled before a file needs it. Asked of a language
        // no other test parses: the tests share one registry, and by now
        // another may have opened a Vue file.
        let untouched = language_for_path(Path::new("a.untouched")).unwrap();
        assert!(!untouched.is_ready());
        assert!(SyntaxTree::parse(untouched.clone(), &Rope::from_str("<p>hi</p>")).is_some());
        assert!(untouched.is_ready());
        let name = |path: &str| language_for_path(Path::new(path)).map(|l| l.name);
        assert_eq!(name("src/Dockerfile"), Some("Dockerfile"));
        assert_eq!(name("api.dockerfile"), Some("Dockerfile"));
        assert_eq!(name("notadockerfile"), None);
        assert_eq!(name("revue"), None);
        // Built-in languages are not replaced.
        assert_eq!(name("a.rs"), Some("Rust"));
        // Asking again keeps the same language, compiled grammar included.
        assert!(Arc::ptr_eq(&vue, &self::vue()));
    }

    #[test]
    fn a_wasm_grammar_highlights_and_injects_other_languages() {
        let rope = Rope::from_str(COMPONENT);
        let tree = SyntaxTree::parse(vue(), &rope).unwrap();
        assert!(tree.language().is_ready());
        assert!(!tree.tree.root_node().has_error());
        let spans = kinds(&tree, &rope, COMPONENT);
        // Vue's own query.
        assert!(spans.contains(&("template".into(), HighlightKind::Tag)));
        assert!(spans.contains(&("class".into(), HighlightKind::Attribute)));
        assert_eq!(tree.language().ids(), ["vue.js", "vue"]);
        let at = |text: &str| tree.language_at(COMPONENT.find(text).unwrap()).name;
        assert_eq!(at("class="), "Vue.js");
        assert_eq!(at("const msg"), "TypeScript");
        assert_eq!(at("color"), "CSS");
        // The script is TypeScript, the style is CSS, and so is the
        // expression of an event handler.
        assert!(spans.contains(&("const".into(), HighlightKind::Keyword)));
        assert!(spans.contains(&("string".into(), HighlightKind::Type)));
        assert!(spans.contains(&("'hi'".into(), HighlightKind::String)));
        assert!(spans.contains(&("color".into(), HighlightKind::Property)));
        assert!(spans.contains(&("go".into(), HighlightKind::Function)));
        assert!(spans.contains(&("1".into(), HighlightKind::Number)));
    }

    #[test]
    fn injected_languages_follow_edits() {
        let mut buffer = Buffer::new(COMPONENT);
        let mut tree = SyntaxTree::parse(vue(), buffer.rope()).unwrap();
        let at = COMPONENT.find("</script>").unwrap();
        for e in buffer.edit(
            [(at..at, "function twice(n: number) { return n * 2 }\n")],
            &[],
            Instant::now(),
        ) {
            tree.edit(&e);
        }
        assert!(tree.reparse_within(buffer.rope(), Duration::from_secs(5)));
        let src = buffer.rope().to_string();
        let spans = kinds(&tree, buffer.rope(), &src);
        assert!(spans.contains(&("function".into(), HighlightKind::Keyword)));
        assert!(spans.contains(&("number".into(), HighlightKind::Type)));
        // The style after the edit moved with it.
        assert!(spans.contains(&("color".into(), HighlightKind::Property)));
        // Removing the script removes its layer.
        let script = src.find("<script").unwrap()..src.find("<style>").unwrap();
        for e in buffer.edit([(script, "")], &[], Instant::now()) {
            tree.edit(&e);
        }
        let fresh = tree.reparse_in_background(buffer.rope()).unwrap();
        let src = buffer.rope().to_string();
        let spans = kinds(&fresh, buffer.rope(), &src);
        assert!(!spans.iter().any(|(text, _)| text == "const"));
        assert!(spans.contains(&("color".into(), HighlightKind::Property)));
    }

    const TEMPLATE: &str = "<template>\n  <div class=\"box\">\n    <p>{{ msg }}</p>\n  </div>\n  <!-- a \"note\" -->\n</template>\n";

    #[test]
    fn an_extensions_queries_find_brackets_and_scopes() {
        let rope = Rope::from_str(TEMPLATE);
        let tree = SyntaxTree::parse(vue(), &rope).unwrap();
        let at = |what: &str| TEMPLATE.find(what).unwrap();
        let pair = |offset: usize| {
            let (open, close) = tree.brackets_at(&rope, offset).unwrap()?;
            Some((open.start, &TEMPLATE[open], close.start, &TEMPLATE[close]))
        };
        // On a bracket, and right after its other half.
        let div = at("<div");
        let div_end = at("\">") + 1;
        assert_eq!(pair(div), Some((div, "<", div_end, ">")));
        assert_eq!(pair(div_end + 1), Some((div, "<", div_end, ">")));
        // Brackets longer than a character.
        let open = at("{{");
        assert_eq!(pair(open + 1), Some((open, "{{", at("}}"), "}}")));
        assert_eq!(
            pair(at("</div")),
            Some((at("</div"), "</", at("</div") + 5, ">"))
        );
        // Between two: the one under the cursor, not the one behind it.
        let p = at("<p>");
        assert_eq!(pair(p + 3), Some((open, "{{", at("}}"), "}}")));
        // Nothing in the middle of a word.
        assert_eq!(pair(at("box") + 1), None);
        // Right after `{{` the script inside has begun, and the bracket is
        // still the template's.
        assert_eq!(pair(open + 2), Some((open, "{{", at("}}"), "}}")));
        // Inside the script, the script's language answers, and it has no
        // query: the caller looks at the characters.
        assert_eq!(tree.brackets_at(&rope, at("msg") + 1), None);
        // A language with no query for it says so, and the caller looks at
        // the characters instead.
        let js = language_for_path(Path::new("a.js")).unwrap();
        let rope_js = Rope::from_str("f(1)");
        let tree_js = SyntaxTree::parse(js, &rope_js).unwrap();
        assert_eq!(tree_js.brackets_at(&rope_js, 1), None);

        let scope = |offset: usize, name: &str| tree.in_scope(&rope, offset, &[name.to_string()]);
        let value = at("\"box\"");
        assert!(scope(value + 2, "string"));
        assert!(!scope(value + 2, "comment"));
        // Before the opening quote is not yet inside.
        assert!(!scope(value, "string"));
        assert!(scope(at("note"), "comment"));
        assert!(!scope(at("msg"), "string"));
        assert!(!tree_js.in_scope(&rope_js, 2, &["string".to_string()]));
    }

    #[test]
    fn an_extensions_query_says_how_deep_a_line_goes() {
        let rope = Rope::from_str(TEMPLATE);
        let tree = SyntaxTree::parse(vue(), &rope).unwrap();
        let at = |what: &str| TEMPLATE.find(what).unwrap();
        assert!(tree.indents_at(at("<div")));
        let indent = |line: Line| tree.indent(&rope, line, false, false);
        // Enter after an opening tag: one level deeper than the tag.
        let div = at("<div");
        let cut = at("\">") + 2;
        let line = Line {
            above_row: 1,
            above_start: div,
            cut,
            start: cut,
        };
        assert_eq!(
            indent(line),
            Indent {
                row: 1,
                levels: 1,
                in_error: false
            }
        );
        // Enter between a tag and its end: the end goes under the tag, and
        // it is the pair in `config.toml` that asks for a line in between.
        let p = at("<p>");
        let line = Line {
            above_row: 2,
            above_start: p,
            cut: p + 3,
            start: p + 3,
        };
        assert_eq!(
            indent(Line {
                cut: at("{{"),
                start: at("{{"),
                ..line
            }),
            Indent {
                row: 2,
                levels: 1,
                in_error: false
            }
        );
        let empty = Rope::from_str("<template>\n  <p></p>\n</template>\n");
        let empty_tree = SyntaxTree::parse(vue(), &empty).unwrap();
        let line = Line {
            above_row: 1,
            above_start: 13,
            cut: 16,
            start: 16,
        };
        assert_eq!(
            empty_tree.indent(&empty, line, false, false),
            Indent {
                row: 1,
                levels: 0,
                in_error: false
            }
        );
        // After a line that closed its own tag: as deep as that line.
        let cut = at("</p>") + 4;
        let line = Line {
            above_row: 2,
            above_start: p,
            cut,
            start: cut,
        };
        assert_eq!(
            indent(line),
            Indent {
                row: 2,
                levels: 0,
                in_error: false
            }
        );
        // A closing tag on a line of its own goes back to the row that
        // opened it.
        let close = at("</div>");
        let line = Line {
            above_row: 2,
            above_start: p,
            cut: close - 2,
            start: close,
        };
        assert_eq!(
            indent(line),
            Indent {
                row: 1,
                levels: 0,
                in_error: false
            }
        );
        // What the patterns of `config.toml` answered counts next to the
        // query: deeper, back, or both at once, which cancel out.
        let plain = Line {
            above_row: 4,
            above_start: at("<!--"),
            cut: at("</template>"),
            start: at("</template>"),
        };
        let after_comment = |increase, decrease| {
            let line = Line {
                cut: at("-->") + 3,
                start: at("-->") + 3,
                ..plain
            };
            tree.indent(&rope, line, increase, decrease)
        };
        assert_eq!(
            after_comment(false, false),
            Indent {
                row: 4,
                levels: 0,
                in_error: false
            }
        );
        assert_eq!(
            after_comment(true, false),
            Indent {
                row: 4,
                levels: 1,
                in_error: false
            }
        );
        assert_eq!(
            after_comment(false, true),
            Indent {
                row: 4,
                levels: -1,
                in_error: false
            }
        );
        assert_eq!(
            after_comment(true, true),
            Indent {
                row: 4,
                levels: 0,
                in_error: false
            }
        );
        // The end of the template, as it stands: back to the row it opened on.
        assert_eq!(
            indent(plain),
            Indent {
                row: 0,
                levels: 0,
                in_error: false
            }
        );
    }

    #[test]
    fn an_extensions_query_lists_what_a_file_declares() {
        vue();
        let symbols = outline(Path::new("App.vue"), TEMPLATE, 16, || false);
        let seen: Vec<(&str, &str, usize)> = symbols
            .iter()
            .map(|s| (&*s.kind, s.name.as_str(), s.line))
            .collect();
        assert_eq!(seen, [("<", "div", 2), ("<", "p", 3)]);
        assert_eq!(
            outline(Path::new("App.vue"), TEMPLATE, 1, || false).len(),
            1
        );
        let vue = language_for_path(Path::new("App.vue")).unwrap();
        assert!(vue.has_outline());
        assert!(
            !language_for_path(Path::new("a.untouched"))
                .unwrap()
                .has_outline()
        );
    }

    #[test]
    fn a_grammar_that_cannot_be_loaded_gives_no_tree() {
        vue();
        let language = language_for_path(Path::new("Dockerfile")).unwrap();
        assert!(SyntaxTree::parse(language.clone(), &Rope::from_str("FROM a\n")).is_none());
        // The failure is remembered: the file is not read again on every key.
        assert!(language.is_ready());
    }

    #[test]
    fn wasm_parsers_work_from_several_threads() {
        let vue = vue();
        let threads: Vec<_> = (0..6)
            .map(|i| {
                let vue = vue.clone();
                std::thread::spawn(move || {
                    let src = COMPONENT.repeat(20 + i);
                    let rope = Rope::from_str(&src);
                    let tree = SyntaxTree::parse(vue, &rope).unwrap();
                    tree.highlights(&rope, 0..rope.len_bytes()).len()
                })
            })
            .collect();
        for thread in threads {
            assert!(thread.join().unwrap() > 100);
        }
    }
}
