//! JSON as editors write their config: with comments and trailing commas.

use serde_json::Value;

/// `source` with comments blanked and trailing commas dropped, the rest of
/// the text where it was.
fn clean(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b);
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1]);
                i += 1;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match (b, bytes.get(i + 1)) {
            (b'"', _) => {
                in_string = true;
                out.push(b);
                i += 1;
            }
            (b'/', Some(b'/')) => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    // Line numbers in errors stay right.
                    if bytes[i] == b'\n' {
                        out.push(b'\n');
                    }
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            (b',', _) => {
                // Dropped if only space and comments stand before a closer.
                let rest = clean_ahead(&bytes[i + 1..]);
                if !matches!(rest, Some(b'}' | b']')) {
                    out.push(b);
                }
                i += 1;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The next byte that is not space or inside a comment.
fn clean_ahead(bytes: &[u8]) -> Option<u8> {
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1)) {
            (b, _) if b.is_ascii_whitespace() => i += 1,
            (b'/', Some(b'/')) => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            (b, _) => return Some(b),
        }
    }
    None
}

pub fn parse(source: &str) -> Result<Value, String> {
    let cleaned = clean(source);
    if cleaned.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(&cleaned).map_err(|e| e.to_string())
}

/// `source`, a JSON object with comments, with the top-level `key` set to
/// `value`. Everything else stays as written: the user's comments, order
/// and spacing.
pub fn set_key(source: &str, key: &str, value: &Value) -> String {
    let value = value.to_string();
    let Some(open) = top_level_open(source) else {
        // Not an object (empty, or broken): start one.
        return format!("{{\n  {}: {value}\n}}\n", Value::from(key));
    };
    if let Some(span) = value_span(source, open, key) {
        return format!("{}{value}{}", &source[..span.0], &source[span.1..]);
    }
    let after = open + 1;
    let empty = clean_ahead(&source.as_bytes()[after..]) == Some(b'}');
    let comma = if empty { "" } else { "," };
    format!(
        "{}\n  {}: {value}{comma}{}",
        &source[..after],
        Value::from(key),
        &source[after..]
    )
}

/// Where the object's `{` is, past leading comments.
fn top_level_open(source: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1)) {
            (b, _) if b.is_ascii_whitespace() => i += 1,
            (b'/', Some(b'/')) => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            (b'{', _) => return Some(i),
            _ => return None,
        }
    }
    None
}

/// The byte range of the value of top-level `key` in the object at `open`.
fn value_span(source: &str, open: usize, key: &str) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let wanted = Value::from(key).to_string();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1)) {
            (b'/', Some(b'/')) => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            (b'"', _) => {
                let start = i;
                i = string_end(bytes, i);
                let is_key = depth == 1
                    && source[start..i] == wanted
                    && clean_ahead(&bytes[i..]) == Some(b':');
                if is_key {
                    let colon = i + source[i..].find(':')?;
                    let mut from = colon + 1;
                    while bytes.get(from).is_some_and(u8::is_ascii_whitespace) {
                        from += 1;
                    }
                    return Some((from, value_end(bytes, from)));
                }
            }
            (b'{' | b'[', _) => {
                depth += 1;
                i += 1;
            }
            (b'}' | b']', _) => {
                depth = depth.saturating_sub(1);
                i += 1;
                if depth == 0 {
                    return None;
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Past the string that starts at `start`.
fn string_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Past the value that starts at `from`: a string, a nested value, or a
/// bare word up to the next comma, closer, comment or line end.
fn value_end(bytes: &[u8], from: usize) -> usize {
    match bytes.get(from) {
        Some(b'"') => string_end(bytes, from),
        Some(b'{' | b'[') => {
            let mut depth = 0usize;
            let mut i = from;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' => {
                        i = string_end(bytes, i);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return i + 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            bytes.len()
        }
        _ => {
            let mut i = from;
            while i < bytes.len()
                && !matches!(bytes[i], b',' | b'}' | b']' | b'\n' | b'/')
                && !bytes[i].is_ascii_whitespace()
            {
                i += 1;
            }
            i
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_json_with_comments_and_trailing_commas() {
        let value = parse(
            r#"// The user's settings.
            {
              "editor.fontSize": 14, // px
              /* a block
                 comment */
              "url": "https://example.com/a//b", "note": "a /* b */ c",
              "list": [1, 2, /* three */],
            }"#,
        )
        .unwrap();
        assert_eq!(
            value,
            json!({"editor.fontSize": 14, "url": "https://example.com/a//b",
                   "note": "a /* b */ c", "list": [1, 2]})
        );
        assert_eq!(parse("  // nothing yet\n").unwrap(), json!({}));
        assert!(parse("{ \"a\": }").is_err());
    }

    #[test]
    fn sets_a_key_and_leaves_the_rest_as_written() {
        let source = "// Changes apply as soon as you save.\n{\n  \"theme\": \"system\", // mine\n  \"nested\": { \"theme\": \"x\" },\n  \"buffer_font_size\": 13\n}\n";
        let set = set_key(source, "theme", &json!("Dracula"));
        assert_eq!(set, source.replacen("\"system\"", "\"Dracula\"", 1));
        let set = set_key(&set, "buffer_font_size", &json!(15.0));
        assert!(set.contains("\"buffer_font_size\": 15.0\n}"));
        // A key that is not there goes first; the nested one is not it.
        let added = set_key(source, "indent_size", &json!(2));
        assert!(added.contains("{\n  \"indent_size\": 2,\n  \"theme\": \"system\", // mine"));
        assert_eq!(parse(&added).unwrap()["nested"]["theme"], json!("x"));
        assert_eq!(
            parse(&set_key("{}", "a", &json!(true))).unwrap(),
            json!({"a": true})
        );
        assert_eq!(
            parse(&set_key("", "a", &json!(1))).unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            parse(&set_key(
                "{ \"a\": [1, {\"b\": 2}], \"c\": 3 }",
                "a",
                &json!("x")
            ))
            .unwrap(),
            json!({"a": "x", "c": 3})
        );
    }
}
