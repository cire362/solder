//! Text-level helpers: which statement the cursor is in, and quoting.

use std::ops::Range;

use crate::Engine;

/// Byte ranges of the statements in `text`, split on `;` outside strings,
/// quoted identifiers, comments and Postgres dollar quotes. Ranges exclude
/// the `;` and surrounding whitespace; empty statements are dropped.
pub fn statements(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(bytes, i),
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = memchr(b'\n', bytes, i).unwrap_or(bytes.len())
            }
            b'#' if is_line_comment_hash(bytes, i) => {
                i = memchr(b'\n', bytes, i).unwrap_or(bytes.len())
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |end| i + 2 + end + 2)
            }
            b'$' => i = skip_dollar_quote(text, i),
            b';' => {
                push_trimmed(text, start..i, &mut out);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    push_trimmed(text, start..text.len(), &mut out);
    out
}

/// MySQL's `#` comments only; `#` inside Postgres operators (`#>`) follows
/// something other than whitespace or a line start.
fn is_line_comment_hash(bytes: &[u8], i: usize) -> bool {
    (i == 0 || bytes[i - 1].is_ascii_whitespace())
        && bytes.get(i + 1).is_none_or(|b| b.is_ascii_whitespace())
}

fn memchr(needle: u8, bytes: &[u8], from: usize) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|b| *b == needle)
        .map(|p| from + p)
}

fn skip_quoted(bytes: &[u8], open: usize) -> usize {
    let quote = bytes[open];
    let mut i = open + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && quote != b'`' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            // A doubled quote is an escaped quote.
            if bytes.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_dollar_quote(text: &str, open: usize) -> usize {
    let rest = &text[open + 1..];
    let tag_len = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    // `$1` is a parameter, not a quote.
    if !rest[tag_len..].starts_with('$')
        || rest[..tag_len].starts_with(|c: char| c.is_ascii_digit())
    {
        return open + 1;
    }
    let tag = &text[open..open + tag_len + 2];
    let body = open + tag.len();
    text[body..]
        .find(tag)
        .map_or(text.len(), |end| body + end + tag.len())
}

fn push_trimmed(text: &str, range: Range<usize>, out: &mut Vec<Range<usize>>) {
    let slice = &text[range.clone()];
    let start = range.start + (slice.len() - slice.trim_start().len());
    let end = range.start + slice.trim_end().len();
    if start < end && !only_comments(&text[start..end]) {
        out.push(start..end);
    }
}

fn only_comments(s: &str) -> bool {
    s.lines().all(|l| {
        let l = l.trim();
        l.is_empty() || l.starts_with("--")
    })
}

/// The statement to run for a cursor at `offset`: the one containing it, or
/// the closest one before it (a cursor after `;` at the end of a line).
pub fn statement_at(engine: Engine, text: &str, offset: usize) -> Option<Range<usize>> {
    let ranges = match engine {
        Engine::Redis => lines(text),
        Engine::Mongo => blocks(text),
        _ => statements(text),
    };
    ranges
        .iter()
        .find(|r| r.start <= offset && offset <= r.end)
        .or_else(|| ranges.iter().rev().find(|r| r.end <= offset))
        .or(ranges.first())
        .cloned()
}

/// Redis: one command per line.
fn lines(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let range = start..start + line.len();
        start = range.end;
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with('#') {
            push_trimmed(text, range, &mut out);
        }
    }
    out
}

/// Mongo: calls separated by `;` or blank lines, so a multi-line filter
/// stays one statement.
fn blocks(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for statement in statements(text) {
        let mut start = statement.start;
        let mut offset = statement.start;
        for line in text[statement.clone()].split_inclusive('\n') {
            if line.trim().is_empty() {
                push_trimmed(text, start..offset, &mut out);
                start = offset + line.len();
            }
            offset += line.len();
        }
        push_trimmed(text, start..statement.end, &mut out);
    }
    out
}

pub fn quote_ident(engine: Engine, name: &str) -> String {
    match engine {
        Engine::MySql => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

pub fn qualified_name(engine: Engine, namespace: Option<&str>, name: &str) -> String {
    match namespace {
        // Postgres' default schema reads better unqualified.
        Some(ns) if !(engine == Engine::Postgres && ns == "public") => {
            format!("{}.{}", quote_ident(engine, ns), quote_ident(engine, name))
        }
        _ => quote_ident(engine, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str) -> Vec<&str> {
        statements(text).into_iter().map(|r| &text[r]).collect()
    }

    #[test]
    fn splits_outside_strings_comments_and_dollar_quotes() {
        let text = "select 'a;b', \"x;y\" from t; -- c;d\n\
                    insert into t values ($1);\n\
                    create function f() returns int as $body$ select 1; $body$ language sql;\n\
                    /* x; */ select `a;b`\n;\n  ;";
        assert_eq!(
            texts(text),
            vec![
                "select 'a;b', \"x;y\" from t",
                "-- c;d\ninsert into t values ($1)",
                "create function f() returns int as $body$ select 1; $body$ language sql",
                "/* x; */ select `a;b`",
            ]
        );
        assert_eq!(
            texts("select 'it''s; fine'; select 2"),
            vec!["select 'it''s; fine'", "select 2"]
        );
        assert_eq!(
            texts("select data #> '{a}' from t; -- only\n"),
            vec!["select data #> '{a}' from t"]
        );
    }

    #[test]
    fn statement_under_the_cursor() {
        let text = "select 1;\nselect 2;\n\nselect 3";
        let at = |offset| statement_at(Engine::Postgres, text, offset).map(|r| &text[r]);
        assert_eq!(at(3), Some("select 1"));
        // Just after the `;` at the end of the line.
        assert_eq!(at(9), Some("select 1"));
        assert_eq!(at(12), Some("select 2"));
        // On the blank line after it.
        assert_eq!(at(20), Some("select 2"));
        assert_eq!(at(text.len()), Some("select 3"));
        assert_eq!(statement_at(Engine::Postgres, "  \n", 1), None);
    }

    #[test]
    fn redis_lines_and_mongo_blocks() {
        let text = "SET a 1\n# note\nGET a\n";
        let at = |offset| statement_at(Engine::Redis, text, offset).map(|r| &text[r]);
        assert_eq!(at(2), Some("SET a 1"));
        assert_eq!(at(16), Some("GET a"));
        let text = "db.users.find({\n  age: 3\n})\n\ndb.orders.countDocuments({})";
        let at = |offset| statement_at(Engine::Mongo, text, offset).map(|r| &text[r]);
        assert_eq!(at(18), Some("db.users.find({\n  age: 3\n})"));
        assert_eq!(at(text.len()), Some("db.orders.countDocuments({})"));
    }

    #[test]
    fn quoting() {
        assert_eq!(
            qualified_name(Engine::Postgres, Some("public"), "users"),
            "\"users\""
        );
        assert_eq!(
            qualified_name(Engine::Postgres, Some("app"), "we\"ird"),
            "\"app\".\"we\"\"ird\""
        );
        assert_eq!(
            qualified_name(Engine::MySql, Some("shop"), "orders"),
            "`shop`.`orders`"
        );
    }
}
