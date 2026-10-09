use std::{
    borrow::Cow,
    ops::ControlFlow,
    path::Path,
    time::{Duration, Instant},
};

use tree_sitter::{Node, ParseOptions, ParseState, QueryCursor, StreamingIterator};

#[derive(Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    /// `fn`, `type` or `const` for a built-in language. For an extension's,
    /// the word its outline query shows before the name (`def`, `class`),
    /// or `item` when it shows none.
    pub kind: Cow<'static, str>,
    pub line: usize,
}

pub fn outline(
    path: &Path,
    source: &str,
    limit: usize,
    cancelled: impl Fn() -> bool,
) -> Vec<Symbol> {
    with_budget(path, source, limit, cancelled, Duration::from_millis(25))
}

fn with_budget(
    path: &Path,
    source: &str,
    limit: usize,
    cancelled: impl Fn() -> bool,
    budget: Duration,
) -> Vec<Symbol> {
    if limit == 0 || cancelled() {
        return Vec::new();
    }
    let Some(language) = super::language_for_path(path) else {
        return Vec::new();
    };
    let Some(mut parser) = super::new_parser(&language) else {
        return Vec::new();
    };
    // Lazy queries can take longer to compile than the outline's whole
    // budget. That one-time load must not leave the first outline empty.
    let rules = language.rules();
    let deadline = Instant::now() + budget;
    let mut read = |offset: usize, _| source.as_bytes().get(offset..).unwrap_or_default();
    let mut stop = |_: &ParseState| {
        if cancelled() || Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut stop);
    let tree = if parser.pooled {
        // An extension's grammar does not parse on this thread.
        let rope = ropey::Rope::from_str(source);
        super::parse_rope(&language, &mut parser, &rope, None, Some(deadline))
    } else {
        parser.parse_with_options(&mut read, None, Some(options))
    };
    let Some(tree) = tree else {
        return Vec::new();
    };
    let mut symbols = Vec::new();
    // An extension's language says what its declarations are in a query.
    if let Some(outline) = &rules.outline {
        let text = |node: Node<'_>| node.utf8_text(source.as_bytes()).ok();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&outline.query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches.next() {
            if cancelled() || Instant::now() >= deadline || symbols.len() >= limit {
                break;
            }
            let Some(item) = m.nodes_for_capture_index(outline.item).next() else {
                continue;
            };
            let name: Vec<&str> = m
                .nodes_for_capture_index(outline.name)
                .filter_map(text)
                .collect();
            if name.is_empty() {
                continue;
            }
            let kind = outline
                .context
                .and_then(|context| m.nodes_for_capture_index(context).find_map(text))
                .and_then(|context| context.split_whitespace().next())
                .map_or(Cow::Borrowed("item"), |word| {
                    Cow::Owned(word.chars().take(16).collect())
                });
            symbols.push(Symbol {
                name: name.join(" ").chars().take(120).collect(),
                kind,
                line: item.start_position().row + 1,
            });
        }
        return symbols;
    }
    let mut cursor = tree.walk();
    loop {
        if cancelled() || Instant::now() >= deadline || symbols.len() >= limit {
            break;
        }
        let node = cursor.node();
        if let Some(kind) = declaration_kind(node)
            && let Some(name) = name_of(node)
            && let Ok(name_text) = name.utf8_text(source.as_bytes())
        {
            symbols.push(Symbol {
                name: name_text.trim().chars().take(120).collect(),
                kind: Cow::Borrowed(kind),
                line: node.start_position().row + 1,
            });
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return symbols;
            }
        }
    }
    symbols
}

/// The node that holds what a declaration is called.
fn name_of(node: Node<'_>) -> Option<Node<'_>> {
    if let Some(name) = node.child_by_field_name("name") {
        return matches!(
            name.kind(),
            "identifier"
                | "type_identifier"
                | "property_identifier"
                | "field_identifier"
                // A shell function.
                | "word"
        )
        .then_some(name);
    }
    // A heading of a Markdown file is called what it says.
    if let Some(heading) = node.child_by_field_name("heading_content") {
        return Some(heading);
    }
    // C and C++ put the name inside what declares it: `int *run(void)` is
    // a pointer declarator around a function declarator around `run`.
    let mut inner = node.child_by_field_name("declarator")?;
    for _ in 0..8 {
        match inner.kind() {
            "identifier"
            | "field_identifier"
            | "type_identifier"
            | "qualified_identifier"
            | "operator_name"
            | "destructor_name" => return Some(inner),
            _ => inner = inner.child_by_field_name("declarator")?,
        }
    }
    None
}

fn declaration_kind(node: Node<'_>) -> Option<&'static str> {
    match node.kind() {
        "function_item"
        | "function_signature_item"
        | "function_declaration"
        | "function_definition"
        | "method_declaration"
        | "method_definition" => Some("fn"),
        "struct_item"
        | "enum_item"
        | "trait_item"
        | "type_item"
        | "type_spec"
        | "class_declaration"
        | "class_definition"
        | "interface_declaration"
        | "type_alias_declaration"
        | "enum_declaration" => Some("type"),
        // C and C++: only where the type is given its body, not where it
        // is merely used.
        "struct_specifier" | "class_specifier" | "enum_specifier" | "union_specifier"
            if node.child_by_field_name("body").is_some() =>
        {
            Some("type")
        }
        "type_definition" => Some("type"),
        "atx_heading" => Some("#"),
        "const_item" | "static_item" => Some("const"),
        "variable_declarator"
            if node.child_by_field_name("value").is_some_and(|value| {
                matches!(value.kind(), "arrow_function" | "function_expression")
            }) =>
        {
            Some("fn")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_not_bodies_across_languages() {
        let cases = [
            (
                "app.rs",
                "struct App {}\nfn run() { let secret = \"PRIVATE\"; }",
                vec!["App", "run"],
            ),
            (
                "app.ts",
                "interface App {}\nconst run = () => 'PRIVATE';",
                vec!["App", "run"],
            ),
            (
                "app.tsx",
                "class App {}\nfunction run() { return <p>PRIVATE</p>; }",
                vec!["App", "run"],
            ),
            (
                "app.js",
                "class App {}\nfunction run() { return 'PRIVATE'; }",
                vec!["App", "run"],
            ),
            (
                "app.go",
                "package main\ntype App struct {}\nfunc run() { println(\"PRIVATE\") }",
                vec!["App", "run"],
            ),
            (
                "app.py",
                "class App:\n    pass\ndef run():\n    return 'PRIVATE'",
                vec!["App", "run"],
            ),
            (
                "app.c",
                "struct App { int a; };\nstruct App *run(void) { puts(\"PRIVATE\"); return 0; }\nstruct App other;",
                vec!["App", "run"],
            ),
            (
                "app.cpp",
                "class App { public: int run(); };\nint App::run() { return 1; }\ntypedef int Id;",
                vec!["App", "App::run", "Id"],
            ),
            (
                "app.sh",
                "run() { echo PRIVATE; }\nfunction stop { :; }\n",
                vec!["run", "stop"],
            ),
            (
                "app.md",
                "# App\n\nPRIVATE words.\n\n## How to run\n",
                vec!["App", "How to run"],
            ),
        ];
        for (path, source, names) in cases {
            // Declaration correctness must not depend on how much CPU
            // concurrent tests or compiler jobs leave this thread.
            let symbols = with_budget(
                Path::new(path),
                source,
                16,
                || false,
                Duration::from_secs(2),
            );
            assert_eq!(
                symbols
                    .iter()
                    .map(|symbol| symbol.name.as_str())
                    .collect::<Vec<_>>(),
                names,
                "{path}"
            );
            assert_eq!(symbols[0].line, if path.ends_with("go") { 2 } else { 1 });
            assert!(!format!("{symbols:?}").contains("PRIVATE"));
        }
    }

    #[test]
    fn limits_cancellation_and_computed_names() {
        let path = Path::new("app.js");
        let source = "function first() {}\nfunction second() {}\nclass App { ['PRIVATE']() {} }";
        assert_eq!(
            with_budget(path, source, 1, || false, Duration::from_secs(2)).len(),
            1
        );
        assert!(outline(path, source, 16, || true).is_empty());
        assert!(
            !format!(
                "{:?}",
                with_budget(path, source, 16, || false, Duration::from_secs(2))
            )
            .contains("PRIVATE")
        );
        assert!(with_budget(path, source, 16, || false, Duration::ZERO).is_empty());
        assert!(outline(Path::new("data.json"), "{\"PRIVATE\": 1}", 16, || false).is_empty());
    }
}
