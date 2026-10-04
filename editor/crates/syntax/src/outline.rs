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
    if limit == 0 || cancelled() {
        return Vec::new();
    }
    let Some(language) = super::language_for_path(path) else {
        return Vec::new();
    };
    let Some(mut parser) = super::new_parser(&language) else {
        return Vec::new();
    };
    let deadline = Instant::now() + Duration::from_millis(25);
    let mut read = |offset: usize, _| source.as_bytes().get(offset..).unwrap_or_default();
    let mut stop = |_: &ParseState| {
        if cancelled() || Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut stop);
    let Some(tree) = parser.parse_with_options(&mut read, None, Some(options)) else {
        return Vec::new();
    };
    let mut symbols = Vec::new();
    // An extension's language says what its declarations are in a query.
    if let Some(outline) = &language.rules().outline {
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
            && let Some(name) = node.child_by_field_name("name")
            && matches!(
                name.kind(),
                "identifier" | "type_identifier" | "property_identifier" | "field_identifier"
            )
            && let Ok(name_text) = name.utf8_text(source.as_bytes())
        {
            symbols.push(Symbol {
                name: name_text.chars().take(120).collect(),
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
        ];
        for (path, source, names) in cases {
            let symbols = outline(Path::new(path), source, 16, || false);
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
        assert_eq!(outline(path, source, 1, || false).len(), 1);
        assert!(outline(path, source, 16, || true).is_empty());
        assert!(!format!("{:?}", outline(path, source, 16, || false)).contains("PRIVATE"));
        assert!(outline(Path::new("data.json"), "{\"PRIVATE\": 1}", 16, || false).is_empty());
    }
}
