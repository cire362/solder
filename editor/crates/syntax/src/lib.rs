//! Incremental parsing and highlighting on top of tree-sitter.
//!
//! Two rules keep this off the typing path:
//! - grammars and their highlight queries compile lazily, on the first file
//!   that needs them, so startup never pays for languages you do not open;
//! - highlights are queried only for the byte range on screen, so a keystroke
//!   in a 50k-line file costs the same as one in a 50-line file.

use std::{
    collections::{HashMap, VecDeque},
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
            // Markdown has no code to color: a heading stands out like a
            // keyword, what is quoted like a string, and a link's text and
            // the two kinds of emphasis each get a color of their own,
            // since there is no bold or slanted text to give them.
            "text.title" => Keyword,
            "text.literal" | "text.uri" => String,
            "text.reference" => Property,
            "text.strong" => Constant,
            "text.emphasis" => Attribute,
            "delimiter" => Punctuation,
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
        /// The query that finds other languages inside it, for the few
        /// built-in ones that hold any: the code blocks of Markdown.
        injections: Option<&'static str>,
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
    /// The parses of an extension's grammar, which run on a thread of
    /// their own.
    watch: wasm::Watch,
}

struct Highlighter {
    query: Query,
    kinds: Vec<Option<HighlightKind>>,
    /// The capture that takes color away again (`@none`): the inside of a
    /// code block, which the block around it paints as quoted text and the
    /// block's own language then colors.
    none: Option<u32>,
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
}

impl Language {
    /// False while the grammar still has to be compiled, which takes long
    /// enough to stay off the UI thread.
    pub fn is_ready(&self) -> bool {
        self.grammar.get().is_some()
    }

    /// Whether its grammar stopped answering and was given up on: its
    /// files are plain text until the editor starts again.
    pub fn is_hung(&self) -> bool {
        self.watch.is_hung()
    }

    fn grammar(&self) -> Option<&tree_sitter::Language> {
        if self.is_hung() {
            return None;
        }
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
                let none = query.capture_index_for_name("none");
                Some(Highlighter { query, kinds, none })
            })
            .as_ref()
    }

    fn injector(&self) -> Option<&Injector> {
        self.injector
            .get_or_init(|| {
                let source = match &self.source {
                    Source::Native { injections, .. } => (*injections)?.to_string(),
                    Source::Wasm(spec) => {
                        std::fs::read_to_string(spec.injections.as_ref()?).ok()?
                    }
                };
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
                    "C++" => &["cpp"][..],
                    "YAML" => &["yml"][..],
                    "Shell Script" => &["shellscript", "sh", "bash", "zsh"][..],
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
        "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" => "C++",
        "md" => "Markdown",
        // What Markdown calls the grammar of the text inside its blocks.
        "markdown_inline" | "markdown-inline" => "Markdown Inline",
        "yml" => "YAML",
        "sh" | "bash" | "zsh" | "shell" | "shellscript" | "console" => "Shell Script",
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
        lang!($name, $comment, $grammar, inject: None, $($query),+)
    };
    ($name:literal, $comment:literal, $grammar:expr, inject: $injections:expr, $($query:expr),+) => {
        Arc::new(Language {
            name: $name,
            line_comment: (!$comment.is_empty()).then_some($comment),
            source: Source::Native {
                grammar: $grammar.into(),
                query: || [$($query),+].join("\n"),
                injections: $injections,
            },
            grammar: OnceLock::new(),
            highlighter: OnceLock::new(),
            injector: OnceLock::new(),
            rules: OnceLock::new(),
            watch: wasm::Watch::default(),
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
            // The languages Zed has built in, and so are in no extension
            // of its catalog.
            (
                &["c", "h"][..],
                lang!(
                    "C",
                    "//",
                    tree_sitter_c::LANGUAGE,
                    tree_sitter_c::HIGHLIGHT_QUERY
                ),
            ),
            // C++'s query adds to C's, so it goes last.
            (
                &["cc", "cpp", "cxx", "c++", "hh", "hpp", "hxx", "h++", "ino"][..],
                lang!(
                    "C++",
                    "//",
                    tree_sitter_cpp::LANGUAGE,
                    tree_sitter_c::HIGHLIGHT_QUERY,
                    tree_sitter_cpp::HIGHLIGHT_QUERY
                ),
            ),
            (
                &["md", "markdown"][..],
                lang!(
                    "Markdown",
                    "",
                    tree_sitter_md::LANGUAGE,
                    inject: Some(tree_sitter_md::INJECTION_QUERY_BLOCK),
                    tree_sitter_md::HIGHLIGHT_QUERY_BLOCK
                ),
            ),
            // Not a kind of file: what Markdown's paragraphs and headings
            // are written in, found inside it by the query above.
            (
                &[][..],
                lang!(
                    "Markdown Inline",
                    "",
                    tree_sitter_md::INLINE_LANGUAGE,
                    inject: Some(tree_sitter_md::INJECTION_QUERY_INLINE),
                    tree_sitter_md::HIGHLIGHT_QUERY_INLINE
                ),
            ),
            (
                &["yml", "yaml"][..],
                lang!(
                    "YAML",
                    "#",
                    tree_sitter_yaml::LANGUAGE,
                    tree_sitter_yaml::HIGHLIGHTS_QUERY
                ),
            ),
            (
                &["sh", "bash", "zsh"][..],
                lang!(
                    "Shell Script",
                    "#",
                    tree_sitter_bash::LANGUAGE,
                    tree_sitter_bash::HIGHLIGHT_QUERY
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
                    watch: wasm::Watch::default(),
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
    let name = path.file_name()?.to_str()?;
    // Files a shell reads that have no ending to go by.
    if matches!(
        name,
        ".bashrc"
            | ".bash_profile"
            | ".bash_login"
            | ".bash_logout"
            | ".profile"
            | ".zshrc"
            | ".zshenv"
            | ".zprofile"
            | ".zlogin"
            | ".zlogout"
            | "PKGBUILD"
            | "APKBUILD"
    ) {
        return registry()
            .iter()
            .map(|(_, language)| language)
            .find(|language| language.name == "Shell Script")
            .cloned();
    }
    // An extension's suffix is the whole file name (`Dockerfile`) or what
    // follows a dot (`vue`, `blade.php`).
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
    /// The stretch of the text that changed since the trees were parsed.
    /// A layer whose text lies outside it is not parsed again: a Markdown
    /// file has one for every paragraph.
    dirty: Option<Range<usize>>,
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
        Self::parse_until(language, rope, None)
    }

    /// A parse that gives up after `budget`: for the thread that draws,
    /// which finishes elsewhere with [`SyntaxTree::parse`].
    pub fn parse_within(language: Arc<Language>, rope: &Rope, budget: Duration) -> Option<Self> {
        Self::parse_until(language, rope, Some(Instant::now() + budget))
    }

    fn parse_until(
        language: Arc<Language>,
        rope: &Rope,
        deadline: Option<Instant>,
    ) -> Option<Self> {
        let tree = parse_rope(&language, &mut new_parser(&language)?, rope, None, deadline)?;
        // An extension's queries are read from disk and compiled here, where
        // the caller already expects to wait, and not at the first paint.
        if matches!(language.source, Source::Wasm(_)) {
            language.highlighter();
            language.rules();
        }
        let layers = parse_layers(&language, &tree, rope, &[], None, deadline)?;
        Some(Self {
            language,
            tree,
            layers,
            stale: false,
            dirty: None,
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
        // What changed before follows the text; then it grows by this edit.
        let (start, old_end, new_end) = (edit.start_byte, edit.old_end_byte, edit.new_end_byte);
        let moved = |at: usize| {
            if at >= old_end {
                at + new_end - old_end
            } else {
                at.min(new_end)
            }
        };
        self.dirty = Some(match self.dirty.take() {
            Some(dirty) => moved(dirty.start).min(start)..moved(dirty.end).max(new_end),
            None => start..new_end,
        });
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
        let tree = parse_rope(
            &self.language,
            &mut parser,
            rope,
            Some(&self.tree),
            deadline,
        )?;
        drop(parser);
        let layers = parse_layers(
            &self.language,
            &tree,
            rope,
            &self.layers,
            self.dirty.clone(),
            deadline,
        )?;
        Some(SyntaxTree {
            language: self.language.clone(),
            tree,
            layers,
            stale: false,
            dirty: None,
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
        let color = match hl.kinds[capture.index as usize] {
            Some(kind) => kind as u8,
            None if Some(capture.index) == hl.none => NONE,
            None => continue,
        };
        // Captures come in document order, and for one node in the order
        // of their patterns, so writing each over the last gives the rule.
        let node = capture.node.byte_range();
        let start = node.start.max(range.start) - range.start;
        let end = node.end.min(range.end).saturating_sub(range.start);
        if start < end {
            slots[start..end].fill(color);
        }
    }
}

/// The layers a tree had before it was edited, for the new ones to take
/// their trees from.
struct Old<'a> {
    layers: &'a [Layer],
    taken: Vec<bool>,
    /// The layer of a language that covers exactly a stretch of the text,
    /// by where the stretch starts and ends now that the edits moved it.
    places: HashMap<(*const Language, usize, usize), usize>,
    /// For each language, its layers in order, for a piece of text that
    /// changed its length and so has no place to be found by: layers keep
    /// their order while typing, and the first one left is the same piece.
    left: HashMap<*const Language, VecDeque<usize>>,
    /// What changed in the text since they were parsed, if that is known.
    dirty: Option<Range<usize>>,
}

impl<'a> Old<'a> {
    fn new(layers: &'a [Layer], dirty: Option<Range<usize>>) -> Self {
        let mut places = HashMap::new();
        let mut left: HashMap<*const Language, VecDeque<usize>> = HashMap::new();
        for (index, layer) in layers.iter().enumerate() {
            let language = Arc::as_ptr(&layer.language);
            let ranges = layer.tree.included_ranges();
            if let (Some(first), Some(last)) = (ranges.first(), ranges.last()) {
                places
                    .entry((language, first.start_byte, last.end_byte))
                    .or_insert(index);
            }
            left.entry(language).or_default().push_back(index);
        }
        Self {
            layers,
            taken: vec![false; layers.len()],
            places,
            left,
            dirty,
        }
    }

    /// The old layer for the text of `language` at `ranges`.
    fn take(
        &mut self,
        language: &Arc<Language>,
        ranges: &[tree_sitter::Range],
    ) -> Option<&'a Layer> {
        let language = Arc::as_ptr(language);
        let place = (
            language,
            ranges.first()?.start_byte,
            ranges.last()?.end_byte,
        );
        let index = match self.places.get(&place) {
            Some(index) if !self.taken[*index] => *index,
            _ => {
                let left = self.left.get_mut(&language)?;
                loop {
                    let index = left.pop_front()?;
                    if !self.taken[index] {
                        break index;
                    }
                }
            }
        };
        self.taken[index] = true;
        Some(&self.layers[index])
    }

    /// Whether `layer` covers exactly `ranges` and nothing in or next to
    /// them changed: its tree is then right as it stands.
    fn untouched(&self, layer: &Layer, ranges: &[tree_sitter::Range]) -> bool {
        let Some(dirty) = &self.dirty else {
            return false;
        };
        layer.tree.included_ranges() == ranges
            && ranges
                .iter()
                .all(|r| r.end_byte < dirty.start || r.start_byte > dirty.end)
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
    dirty: Option<Range<usize>>,
    deadline: Option<Instant>,
) -> Option<Vec<Layer>> {
    let mut layers = Vec::new();
    if language.injector().is_some() {
        let mut old = Old::new(old, dirty);
        inject(language, tree, rope, &mut old, deadline, 1, &mut layers)?;
    }
    Some(layers)
}

fn inject(
    language: &Arc<Language>,
    tree: &Tree,
    rope: &Rope,
    old: &mut Old,
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
            // The whole node, children and all, which is how Zed reads
            // these queries. Leaving the children out, as tree-sitter's
            // own highlighter does, cuts Markdown's text at every
            // punctuation mark: its block grammar makes a node of each.
            if node.start_byte() < node.end_byte() {
                ranges.push(node.range());
            }
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
        let before = old.take(&target, &ranges);
        let parsed = match before {
            Some(layer) if old.untouched(layer, &ranges) => layer.tree.clone(),
            _ => {
                let Some(mut parser) = new_parser(&target) else {
                    continue;
                };
                if parser.set_included_ranges(&ranges).is_err() {
                    continue;
                }
                let old = before.map(|layer| &layer.tree);
                parse_rope(&target, &mut parser, rope, old, deadline)?
            }
        };
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
                deadline,
                depth + 1,
                layers,
            )?;
        }
    }
    Some(())
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

#[cfg(test)]
thread_local! {
    /// How many parses this thread ran, for the tests that count them.
    static PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The grammars that stopped answering and were given up on, by the name
/// of their language, for the editor to say so.
pub fn hung_grammars() -> Vec<&'static str> {
    HUNG.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

static HUNG: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

/// Parses with `parser`, which is set up for `language`. An extension's
/// grammar does not run on this thread: see `wasm.rs`.
fn parse_rope(
    language: &Language,
    parser: &mut LanguageParser,
    rope: &Rope,
    old: Option<&Tree>,
    deadline: Option<Instant>,
) -> Option<Tree> {
    #[cfg(test)]
    PARSES.with(|parses| parses.set(parses.get() + 1));
    if !parser.pooled {
        return run_parser(parser, rope, old, deadline);
    }
    let mut taken = parser.parser.take()?;
    // Snapshots: both are shared underneath, not copied.
    let (rope, old) = (rope.clone(), old.cloned());
    let outcome = language.watch.run(deadline, move || {
        let tree = run_parser(&mut taken, &rope, old.as_ref(), deadline);
        (taken, tree)
    });
    match outcome {
        wasm::Outcome::Done((back, tree)) => {
            parser.parser = Some(back);
            tree
        }
        wasm::Outcome::Late => None,
        wasm::Outcome::Hung => {
            let mut hung = HUNG.lock().unwrap_or_else(|e| e.into_inner());
            if !hung.contains(&language.name) {
                eprintln!("the grammar of {} stopped answering", language.name);
                hung.push(language.name);
            }
            None
        }
    }
}

fn run_parser(
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
    fn a_grammar_that_does_not_answer_is_given_up_on() {
        let vue = vue();
        let rope = Rope::from_str(COMPONENT);
        // An extension's grammar parses on a thread of its own, and the
        // tree is the same as ever.
        let tree = SyntaxTree::parse(vue.clone(), &rope).unwrap();
        assert!(!tree.highlights(&rope, 0..rope.len_bytes()).is_empty());
        assert!(wasm::THREADS.load(std::sync::atomic::Ordering::Relaxed) > 0);
        assert!(!vue.is_hung() && !hung_grammars().contains(&"Vue.js"));

        // The same grammar under a language with no patience at all, in
        // place of one whose scanner never returns: its first parse is
        // not waited for, and that is the last one it is asked for.
        let Source::Wasm(spec) = &vue.source else {
            unreachable!()
        };
        let impatient = Arc::new(Language {
            name: "Impatient",
            line_comment: None,
            source: Source::Wasm(spec.clone()),
            grammar: OnceLock::new(),
            highlighter: OnceLock::new(),
            injector: OnceLock::new(),
            rules: OnceLock::new(),
            watch: wasm::Watch::patient(Duration::ZERO),
        });
        assert!(SyntaxTree::parse(impatient.clone(), &rope).is_none());
        assert!(impatient.is_hung());
        assert_eq!(hung_grammars(), ["Impatient"]);
        // Its files are plain text from then on, at no cost.
        let asked = Instant::now();
        assert!(SyntaxTree::parse(impatient.clone(), &rope).is_none());
        assert!(SyntaxTree::parse_within(impatient, &rope, Duration::from_secs(5)).is_none());
        assert!(asked.elapsed() < Duration::from_millis(200));
        // The others go on as before.
        assert!(SyntaxTree::parse(vue, &rope).is_some());
    }

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

    /// What `path` is called and how `src` is colored as that language.
    fn painted(path: &str, src: &str) -> (&'static str, Vec<(String, HighlightKind)>) {
        let language = language_for_path(Path::new(path)).unwrap();
        let rope = Rope::from_str(src);
        let tree = SyntaxTree::parse(language.clone(), &rope).unwrap();
        (language.name, kinds(&tree, &rope, src))
    }

    #[test]
    fn the_languages_zed_has_built_in_are_built_in_here() {
        use HighlightKind::*;
        let has = |spans: &[(std::string::String, HighlightKind)], text: &str, kind| {
            assert!(
                spans.contains(&(text.to_string(), kind)),
                "{text}: {spans:?}"
            );
        };
        let (name, spans) = painted(
            "main.c",
            "#include <stdio.h>\nint main(void) { return puts(\"hi\"); }\n",
        );
        assert_eq!(name, "C");
        has(&spans, "return", Keyword);
        has(&spans, "\"hi\"", String);
        has(&spans, "puts", Function);
        assert_eq!(painted("util.h", "int a;").0, "C");

        // C++ is C's colors with its own on top.
        let (name, spans) = painted(
            "shape.cpp",
            "namespace geo { class Shape { public: virtual int area() const { return 1; } }; }\n",
        );
        assert_eq!(name, "C++");
        has(&spans, "namespace", Keyword);
        has(&spans, "class", Keyword);
        has(&spans, "return", Keyword);
        has(&spans, "1", Number);
        assert_eq!(painted("shape.hpp", "class A {};").0, "C++");

        let (name, spans) = painted("ci.yml", "name: build # all of it\nsteps:\n  - run: true\n");
        assert_eq!(name, "YAML");
        has(&spans, "name", Property);
        has(&spans, "# all of it", Comment);
        has(&spans, "true", Constant);
        assert_eq!(painted("a.yaml", "a: 1").0, "YAML");

        let (name, spans) = painted(
            "build.sh",
            "#!/bin/sh\nfor f in *.rs; do echo \"$f\"; done # all\n",
        );
        assert_eq!(name, "Shell Script");
        has(&spans, "for", Keyword);
        has(&spans, "echo", Function);
        has(&spans, "# all", Comment);
        // The files a shell reads have no ending to go by.
        assert_eq!(painted("home/.zshrc", "export A=1").0, "Shell Script");
        assert_eq!(painted(".bash_profile", "export A=1").0, "Shell Script");
        assert!(language_for_path(Path::new("zshrc")).is_none());
        // Comments are what the editor's own comment key writes.
        let comment = |path: &str| language_for_path(Path::new(path)).unwrap().line_comment;
        assert_eq!(comment("a.c"), Some("//"));
        assert_eq!(comment("a.cc"), Some("//"));
        assert_eq!(comment("a.yml"), Some("#"));
        assert_eq!(comment("a.sh"), Some("#"));
        assert_eq!(comment("a.md"), None);
    }

    const NOTES: &str = "# Notes on `run`\n\nSome *slanted* and **strong** text with a [link](https://example.com).\n\n```rust\nfn main() { let x = \"hi\"; }\n```\n\n```nothing-known\nplain words\n```\n\n- an item\n";

    #[test]
    fn markdown_is_two_grammars_and_the_languages_in_its_blocks() {
        use HighlightKind::*;
        let (name, spans) = painted("README.md", NOTES);
        assert_eq!(name, "Markdown");
        let has = |text: &str, kind| {
            assert!(
                spans.contains(&(text.to_string(), kind)),
                "{text}: {spans:?}"
            );
        };
        // The block grammar: the heading, its marker, the list's marker.
        has("#", Punctuation);
        has("Notes on ", Keyword);
        has("- ", Punctuation);
        // The inline grammar, inside the paragraph and inside the heading.
        has("run", String);
        has("slanted", Attribute);
        has("strong", Constant);
        has("link", Property);
        has("https://example.com", String);
        // A block of code is colored by its own language, and what that
        // language leaves alone is not painted as quoted text.
        has("fn", Keyword);
        has("\"hi\"", String);
        has("main", Function);
        assert!(
            !spans
                .iter()
                .any(|(text, kind)| *kind == String && text.contains("x =")),
            "{spans:?}"
        );
        // A block in a language nobody knows is plain.
        assert!(
            !spans.iter().any(|(text, _)| text.contains("plain words")),
            "{spans:?}"
        );

        let rope = Rope::from_str(NOTES);
        let tree = SyntaxTree::parse(language_for_path(Path::new("a.md")).unwrap(), &rope).unwrap();
        let at = |what: &str| NOTES.find(what).unwrap();
        assert_eq!(tree.language_at(at("fn main")).name, "Rust");
        assert_eq!(tree.language_at(at("slanted")).name, "Markdown Inline");
        assert_eq!(tree.language_at(at("```rust")).name, "Markdown");
        // The inline grammar is not a kind of file.
        assert!(language_for_path(Path::new("a.markdown_inline")).is_none());
    }

    #[test]
    fn a_paragraph_that_did_not_change_is_not_parsed_again() {
        // Many paragraphs, each a piece of the inline grammar of its own.
        let src: String = (0..40)
            .map(|i| format!("Paragraph *{i}* here.\n\n"))
            .collect();
        let mut buffer = Buffer::new(&src);
        let language = language_for_path(Path::new("a.md")).unwrap();
        let mut tree = SyntaxTree::parse(language, buffer.rope()).unwrap();
        assert_eq!(tree.layers.len(), 40);
        let parses = || PARSES.with(|parses| parses.replace(0));
        // The file's own tree and one for each paragraph.
        assert_eq!(parses(), 41);

        // A word typed into the twentieth.
        let at = src.find("*20*").unwrap();
        for e in buffer.edit([(at..at, "new ")], &[], Instant::now()) {
            tree.edit(&e);
        }
        assert!(tree.is_stale());
        assert!(tree.reparse_within(buffer.rope(), Duration::from_secs(5)));
        assert_eq!(tree.layers.len(), 40);
        // The file and that one paragraph: every other kept its tree as it
        // was, before the edit and after it, where the text only moved.
        assert_eq!(parses(), 2);
        // And all of them are colored where the text now is.
        let text = buffer.rope().to_string();
        let spans = kinds(&tree, buffer.rope(), &text);
        let slanted = spans
            .iter()
            .filter(|(_, kind)| *kind == HighlightKind::Attribute)
            .count();
        assert_eq!(slanted, 40, "{spans:?}");
        assert!(spans.contains(&("39".into(), HighlightKind::Attribute)));

        // A paragraph taken out: those after it are the same text further
        // up, and one layer fewer.
        let gone = text.find("Paragraph *5*").unwrap()..text.find("Paragraph *6*").unwrap();
        for e in buffer.edit([(gone, "")], &[], Instant::now()) {
            tree.edit(&e);
        }
        assert!(tree.reparse_within(buffer.rope(), Duration::from_secs(5)));
        assert_eq!(tree.layers.len(), 39);
        // The file again, and the paragraph that now starts where the one
        // that went did. The rest are found by where they are and kept.
        assert_eq!(parses(), 2);
        let text = buffer.rope().to_string();
        let spans = kinds(&tree, buffer.rope(), &text);
        assert!(spans.contains(&("39".into(), HighlightKind::Attribute)));
        assert!(!spans.contains(&("5".into(), HighlightKind::Attribute)));
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
