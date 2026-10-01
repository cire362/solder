//! `.http` files, as JetBrains IDEs and VS Code's REST Client write them:
//!
//! ```text
//! @baseUrl = http://localhost:3000
//!
//! ### Create a user
//! POST {{baseUrl}}/users
//! Content-Type: application/json
//!
//! {"name": "ada"}
//! ```
//!
//! Requests are separated by `###` lines. `@name = value` defines a
//! variable; `{{name}}` uses one, falling back to the project's `.env`.

use std::{collections::HashMap, ops::Range};

const METHODS: [&str; 9] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// The text after `###`, if any.
    pub name: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// One request in a file, before variables are filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub range: Range<usize>,
    pub name: String,
    /// Byte range of the request line, to put the cursor on.
    pub line: Range<usize>,
}

/// The requests in `text` and the variables it defines.
pub fn blocks(text: &str) -> (Vec<Block>, HashMap<String, String>) {
    let mut blocks = Vec::new();
    let mut vars = HashMap::new();
    let mut start = 0;
    let mut name = String::new();
    let mut request_line: Option<Range<usize>> = None;
    let mut offset = 0;
    fn close(
        range: Range<usize>,
        name: &mut String,
        line: &mut Option<Range<usize>>,
        blocks: &mut Vec<Block>,
    ) {
        if let Some(line) = line.take() {
            blocks.push(Block {
                range,
                name: std::mem::take(name),
                line,
            });
        }
    }
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("###") {
            close(start..offset, &mut name, &mut request_line, &mut blocks);
            start = offset;
            name = title.trim().to_string();
        } else if let Some(def) = trimmed.strip_prefix('@')
            && request_line.is_none()
            && let Some((key, value)) = def.split_once('=')
        {
            vars.insert(key.trim().to_string(), value.trim().to_string());
        } else if request_line.is_none() && !trimmed.is_empty() && !is_comment(trimmed) {
            request_line = Some(offset..offset + line.len());
        }
        offset += raw.len();
    }
    close(start..offset, &mut name, &mut request_line, &mut blocks);
    (blocks, vars)
}

fn is_comment(line: &str) -> bool {
    line.starts_with('#') || line.starts_with("//")
}

/// The request whose block holds `offset`.
pub fn request_at(text: &str, offset: usize) -> Option<Block> {
    let (blocks, _) = blocks(text);
    blocks
        .iter()
        .find(|b| b.range.contains(&offset))
        .or_else(|| blocks.iter().rev().find(|b| b.range.start <= offset))
        .or(blocks.first())
        .cloned()
}

/// The request in `block`, variables filled in from the file, then `env`.
pub fn parse(text: &str, block: &Block, env: &HashMap<String, String>) -> Result<Request, String> {
    let (_, vars) = blocks(text);
    let fill = |s: &str| fill(s, &vars, env);
    let body_text = &text[block.line.start..block.range.end];
    let mut lines = body_text.lines();
    let first = lines.next().unwrap_or_default().trim();
    let (method, url) = match first.split_once(char::is_whitespace) {
        Some((m, rest)) if METHODS.contains(&m.to_ascii_uppercase().as_str()) => {
            (m.to_ascii_uppercase(), rest.trim())
        }
        _ => ("GET".to_string(), first),
    };
    // `GET /path HTTP/1.1`: the version is not part of the URL.
    let url = url.rsplit_once(" HTTP/").map_or(url, |(u, _)| u).trim();
    let mut headers = Vec::new();
    let mut body: Vec<&str> = Vec::new();
    let mut in_body = false;
    for line in lines {
        if in_body {
            body.push(line);
        } else if line.trim().is_empty() {
            in_body = true;
        } else if is_comment(line.trim()) {
        } else if let Some((name, value)) = line.split_once(':') {
            headers.push((fill(name.trim())?, fill(value.trim())?));
        } else {
            return Err(format!("Not a header: {line}"));
        }
    }
    while body.last().is_some_and(|l| l.trim().is_empty()) {
        body.pop();
    }
    let body = (!body.is_empty())
        .then(|| fill(&body.join("\n")))
        .transpose()?;
    Ok(Request {
        name: block.name.clone(),
        method,
        url: fill(url)?,
        headers,
        body,
    })
}

/// Replaces `{{name}}` with its value; an unknown name is an error, so a
/// request never goes out with a placeholder in it.
fn fill(
    s: &str,
    vars: &HashMap<String, String>,
    env: &HashMap<String, String>,
) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    let mut depth = 0;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or("A {{ has no closing }}")?;
        let name = after[..end].trim();
        let value = vars
            .get(name)
            .or_else(|| env.get(name))
            .ok_or_else(|| format!("Unknown variable {{{{{name}}}}}"))?;
        // Variables may refer to other variables.
        depth += 1;
        if depth > 32 {
            return Err("Variables refer to each other in a loop".into());
        }
        out.push_str(&fill(value, vars, env)?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "@baseUrl = http://localhost:3000\n@api = {{baseUrl}}/api\n\n### List users\nGET {{api}}/users?limit=2\nAccept: application/json\n# a comment\n\n### Create\nPOST {{api}}/users HTTP/1.1\nContent-Type: application/json\nAuthorization: Bearer {{TOKEN}}\n\n{\n  \"name\": \"ada\"\n}\n\n\n###\nhttp://example.com/health\n";

    #[test]
    fn finds_requests_and_variables() {
        let (blocks, vars) = blocks(FILE);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].name, "List users");
        assert_eq!(&FILE[blocks[0].line.clone()], "GET {{api}}/users?limit=2");
        assert_eq!(vars["baseUrl"], "http://localhost:3000");
        let at = |needle: &str| request_at(FILE, FILE.find(needle).unwrap()).unwrap().name;
        assert_eq!(at("Accept"), "List users");
        assert_eq!(at("\"ada\""), "Create");
    }

    #[test]
    fn parses_with_variables_headers_and_body() {
        let env: HashMap<String, String> = [("TOKEN".to_string(), "t0k".to_string())].into();
        let (blocks, _) = blocks(FILE);
        let get = parse(FILE, &blocks[0], &env).unwrap();
        assert_eq!(get.method, "GET");
        assert_eq!(get.url, "http://localhost:3000/api/users?limit=2");
        assert_eq!(
            get.headers,
            [("Accept".to_string(), "application/json".to_string())]
        );
        assert_eq!(get.body, None);
        let post = parse(FILE, &blocks[1], &env).unwrap();
        assert_eq!(post.url, "http://localhost:3000/api/users");
        assert_eq!(post.headers[1].1, "Bearer t0k");
        assert_eq!(post.body.as_deref(), Some("{\n  \"name\": \"ada\"\n}"));
        let bare = parse(FILE, &blocks[2], &env).unwrap();
        assert_eq!(
            (bare.method.as_str(), bare.url.as_str()),
            ("GET", "http://example.com/health")
        );
        let err = parse(FILE, &blocks[1], &HashMap::new()).unwrap_err();
        assert_eq!(err, "Unknown variable {{TOKEN}}");
    }
}
