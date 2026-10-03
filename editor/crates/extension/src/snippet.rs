//! Snippet files. Zed took VS Code's format, so one reader serves both.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    /// What is typed to get it.
    pub prefix: String,
    /// The text, with `$1` and `${1:name}` stops as language servers write them.
    pub body: String,
    pub description: String,
    /// Language ids from the snippet's own `scope`; empty means the file's.
    pub scopes: Vec<String>,
}

/// Every snippet of a file. One with several prefixes comes once per prefix.
pub fn parse(text: &str) -> Vec<Snippet> {
    let Ok(Value::Object(entries)) = import::jsonc::parse(text) else {
        return Vec::new();
    };
    let mut snippets = Vec::new();
    for (name, entry) in &entries {
        let body = match &entry["body"] {
            Value::String(body) => body.clone(),
            Value::Array(lines) => lines
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        let prefixes: Vec<&str> = match &entry["prefix"] {
            Value::String(prefix) => vec![prefix],
            Value::Array(prefixes) => prefixes.iter().filter_map(Value::as_str).collect(),
            _ => continue,
        };
        let scopes: Vec<String> = entry["scope"]
            .as_str()
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        for prefix in prefixes.into_iter().filter(|p| !p.is_empty()) {
            snippets.push(Snippet {
                prefix: prefix.to_string(),
                body: body.clone(),
                description: entry["description"].as_str().unwrap_or(name).to_string(),
                scopes: scopes.clone(),
            });
        }
    }
    snippets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_shapes_of_prefix_and_body() {
        let snippets = parse(
            r#"{
                // comments are allowed
                "Component": {
                    "prefix": ["rfc", "component"],
                    "body": ["function ${1:Name}() {", "\treturn $0", "}"],
                    "description": "A function component",
                },
                "Log": { "prefix": "clg", "body": "console.log($1)", "scope": "javascript, TypeScript" },
                "No prefix": { "body": "x" },
                "Empty": { "prefix": "", "body": "x" }
            }"#,
        );
        assert_eq!(snippets.len(), 3);
        let rfc = snippets.iter().find(|s| s.prefix == "rfc").unwrap();
        assert_eq!(rfc.body, "function ${1:Name}() {\n\treturn $0\n}");
        assert_eq!(rfc.description, "A function component");
        assert!(snippets.iter().any(|s| s.prefix == "component"));
        let log = snippets.iter().find(|s| s.prefix == "clg").unwrap();
        assert_eq!(log.description, "Log");
        assert_eq!(log.scopes, ["javascript", "typescript"]);
        assert!(parse("[1, 2]").is_empty());
        assert!(parse("not json").is_empty());
    }
}
