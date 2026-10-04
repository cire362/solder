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

/// Replaces the variables of a snippet body (`$TM_FILENAME_BASE`,
/// `${TM_SELECTED_TEXT:default}`) with what `lookup` gives for them, leaving
/// the numbered stops for the editor. A variable without a value becomes its
/// default, or nothing. Transforms (`${NAME/regex/format/}`) are not applied:
/// the plain value is used.
pub fn resolve(body: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    resolve_into(&chars, lookup, &mut out);
    out
}

fn resolve_into(chars: &[char], lookup: &dyn Fn(&str) -> Option<String>, out: &mut String) {
    let name_start = |c: char| c.is_ascii_alphabetic() || c == '_';
    let name_end = |from: usize| {
        let mut end = from;
        while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        end
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            out.push(c);
            out.push(chars[i + 1]);
            i += 2;
        } else if c == '$' && chars.get(i + 1).copied().is_some_and(name_start) {
            let end = name_end(i + 1);
            let name: String = chars[i + 1..end].iter().collect();
            out.push_str(&lookup(&name).unwrap_or_default());
            i = end;
        } else if c == '$'
            && chars.get(i + 1) == Some(&'{')
            && chars.get(i + 2).copied().is_some_and(name_start)
        {
            let end = name_end(i + 2);
            let name: String = chars[i + 2..end].iter().collect();
            // The closing brace of this variable, past any nested ones.
            let mut close = end;
            let mut depth = 0;
            while close < chars.len() {
                match chars[close] {
                    '\\' => close += 1,
                    '{' => depth += 1,
                    '}' if depth == 0 => break,
                    '}' => depth -= 1,
                    _ => {}
                }
                close += 1;
            }
            let close = close.min(chars.len());
            match lookup(&name).filter(|value| !value.is_empty()) {
                Some(value) => out.push_str(&value),
                None if chars.get(end) == Some(&':') => {
                    resolve_into(&chars[end + 1..close], lookup, out)
                }
                None => {}
            }
            i = close + 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
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

    #[test]
    fn variables_are_filled_and_stops_are_left() {
        let lookup = |name: &str| match name {
            "TM_FILENAME_BASE" => Some("Card".to_string()),
            "TM_SELECTED_TEXT" => Some(String::new()),
            _ => None,
        };
        let resolved = |body: &str| resolve(body, &lookup);
        assert_eq!(
            resolved("export function ${1:${TM_FILENAME_BASE}}($2) {\n\t$0\n}"),
            "export function ${1:Card}($2) {\n\t$0\n}"
        );
        assert_eq!(resolved("$TM_FILENAME_BASE.test"), "Card.test");
        // No value: the default, which may hold another variable.
        assert_eq!(
            resolved("${TM_SELECTED_TEXT:${TM_FILENAME_BASE}!}"),
            "Card!"
        );
        assert_eq!(resolved("a${UNKNOWN}b${UNKNOWN:c}$UNKNOWN"), "abc");
        // A transform is dropped, the value stays.
        assert_eq!(resolved("${TM_FILENAME_BASE/(.*)/${1:/upcase}/}s"), "Cards");
        // Escaped dollars and plain stops pass through.
        assert_eq!(
            resolved("\\$TM_FILENAME_BASE ${2|a,b|} $"),
            "\\$TM_FILENAME_BASE ${2|a,b|} $"
        );
    }
}
