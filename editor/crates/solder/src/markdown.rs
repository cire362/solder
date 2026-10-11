//! Markdown as blocks to draw: what the preview shows of a file.
//!
//! It reads what is written in READMEs and notes: headings, paragraphs,
//! lists with tasks, quotes, fenced and indented code, tables, rules,
//! emphasis, links by address and by name, pictures. It is not all of
//! CommonMark. HTML is kept as it is written, not laid out.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        text: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    Code {
        language: String,
        text: String,
    },
    Quote(Vec<Block>),
    /// The number the first item has, for a numbered list.
    List {
        first: Option<u64>,
        items: Vec<Item>,
    },
    Rule,
    Table {
        head: Vec<Vec<Inline>>,
        align: Vec<Align>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Html(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Whether a task is done, for an item that is one.
    pub done: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Inline {
    Text(String),
    Code(String),
    Emphasis(Vec<Inline>),
    Strong(Vec<Inline>),
    Strike(Vec<Inline>),
    Link { text: Vec<Inline>, to: String },
    Image { alt: String, from: String },
    Break,
}

/// The blocks of `text`, each with the line of the file it starts on.
pub fn parse(text: &str) -> Vec<(usize, Block)> {
    let mut lines: Vec<&str> = text.lines().collect();
    let mut names = HashMap::new();
    // What a name stands for is said anywhere and used everywhere, so
    // the names are read before anything else. Their lines are left
    // empty, which keeps every other line where it was.
    let mut fence: Option<(char, usize)> = None;
    for line in &mut lines {
        match (fence, fence_of(line)) {
            (None, Some((mark, length, _))) => fence = Some((mark, length)),
            (Some((mark, length)), Some((closing, long, rest)))
                if closing == mark && long >= length && rest.trim().is_empty() =>
            {
                fence = None
            }
            (None, None) => {
                if let Some((name, to)) = definition(line) {
                    names.entry(name).or_insert(to);
                    *line = "";
                }
            }
            _ => {}
        }
    }
    let lines: Vec<(usize, &str)> = lines.into_iter().enumerate().collect();
    blocks(&lines, &names)
}

/// `[name]: address`, with an optional title that is not kept.
fn definition(line: &str) -> Option<(String, String)> {
    let indent = line.len() - line.trim_start().len();
    let rest = line.trim_start().strip_prefix('[').filter(|_| indent < 4)?;
    let (name, rest) = rest.split_once("]:")?;
    let to = rest.split_whitespace().next()?;
    let to = to
        .strip_prefix('<')
        .and_then(|to| to.strip_suffix('>'))
        .unwrap_or(to);
    (!name.trim().is_empty() && !name.contains(['[', ']']) && !name.starts_with('^'))
        .then(|| (label(name), to.to_string()))
}

/// A name as it is looked up: letters of either case and any run of
/// spaces are the same name.
fn label(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn indent(line: &str) -> usize {
    line.chars()
        .take_while(|c| matches!(c, ' ' | '\t'))
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// `line` without up to `width` columns of the space it starts with.
fn outdent(line: &str, width: usize) -> &str {
    let mut taken = 0;
    for (at, c) in line.char_indices() {
        let wide = match c {
            ' ' => 1,
            '\t' => 4,
            _ => return &line[at..],
        };
        if taken + wide > width {
            return &line[at..];
        }
        taken += wide;
    }
    ""
}

/// The mark, its length and what follows, if `line` opens or closes a
/// fenced block.
fn fence_of(line: &str) -> Option<(char, usize, &str)> {
    let text = line.trim_start();
    let mark = text.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let length = text.chars().take_while(|c| *c == mark).count();
    let rest = &text[length..];
    (indent(line) < 4 && length >= 3 && !(mark == '`' && rest.contains('`')))
        .then_some((mark, length, rest))
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let text = line.trim_start();
    let level = text.chars().take_while(|c| *c == '#').count();
    let rest = &text[level..];
    if indent(line) >= 4
        || !(1..=6).contains(&level)
        || !(rest.is_empty() || rest.starts_with([' ', '\t']))
    {
        return None;
    }
    // A run of marks at the end closes the heading and is not its text.
    let rest = rest.trim();
    let closed = rest.trim_end_matches('#');
    let rest = match closed.is_empty() || closed.ends_with([' ', '\t']) {
        true => closed.trim_end(),
        false => rest,
    };
    Some((level as u8, rest))
}

fn rule(line: &str) -> bool {
    let mut marks = line.chars().filter(|c| !matches!(c, ' ' | '\t'));
    let Some(mark) = marks.next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    indent(line) < 4 && marks.clone().all(|c| c == mark) && marks.count() >= 2
}

/// What a list item starts with: its number if it has one, how wide its
/// mark is with the space after it, the mark, and the rest of the line.
fn item(line: &str) -> Option<(Option<u64>, usize, char, &str)> {
    let lead = indent(line);
    let text = line.trim_start();
    let (number, mark, after) = match text.chars().next()? {
        mark @ ('-' | '+' | '*') => (None, mark, &text[1..]),
        '0'..='9' => {
            let digits = text.chars().take_while(char::is_ascii_digit).count();
            let mark = text[digits..]
                .chars()
                .next()
                .filter(|c| matches!(c, '.' | ')'))?;
            (
                Some(text[..digits].parse().ok().filter(|_| digits <= 9)?),
                mark,
                &text[digits + 1..],
            )
        }
        _ => return None,
    };
    if lead >= 4 || !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    let width = text.len() - after.len();
    // Text follows one space; more than four is code inside the item.
    let gap = match indent(after) {
        0 => 1,
        gap @ 1..=4 => gap,
        _ => 1,
    };
    Some((number, lead + width + gap, mark, outdent(after, gap)))
}

fn html(line: &str) -> bool {
    let text = line.trim_start();
    let Some(rest) = text.strip_prefix('<') else {
        return false;
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let name: String = rest
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    // An address in angle brackets is a link, not a tag.
    let tag = !name.is_empty()
        && rest[name.len()..]
            .chars()
            .next()
            .is_none_or(|c| matches!(c, ' ' | '>' | '/' | '\t'));
    indent(line) < 4 && (tag || rest.starts_with("!--"))
}

/// The cells of a table's row, without the bars at its ends.
fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let (mut cells, mut cell, mut escaped) = (Vec::new(), String::new(), false);
    for c in line.chars() {
        match c {
            '|' if !escaped => cells.push(std::mem::take(&mut cell)),
            '\\' if !escaped => {
                escaped = true;
                continue;
            }
            _ => {
                if escaped && c != '|' {
                    cell.push('\\');
                }
                cell.push(c);
            }
        }
        escaped = false;
    }
    if !cell.trim().is_empty() {
        cells.push(cell);
    }
    cells
        .into_iter()
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// How each column is set, if `line` is the row under a table's head.
fn alignments(line: &str) -> Option<Vec<Align>> {
    if !line.contains('-') || !line.contains('|') && !line.contains(':') {
        return None;
    }
    cells(line)
        .iter()
        .map(|cell| {
            let dashes = cell.trim_matches(':');
            (!dashes.is_empty() && dashes.chars().all(|c| c == '-')).then_some(
                match (cell.starts_with(':'), cell.ends_with(':')) {
                    (true, true) => Align::Center,
                    (false, true) => Align::Right,
                    _ => Align::Left,
                },
            )
        })
        .collect::<Option<Vec<_>>>()
        .filter(|columns| !columns.is_empty())
}

/// Whether `line` begins a block of its own, so that a paragraph before
/// it has ended.
fn begins(line: &str) -> bool {
    heading(line).is_some()
        || fence_of(line).is_some()
        || rule(line)
        || html(line)
        || line.trim_start().starts_with('>') && indent(line) < 4
        // A number other than one does not break into a paragraph: a
        // line that happens to start with a year is still its text.
        || item(line).is_some_and(|(number, _, _, rest)| {
            number.is_none_or(|number| number == 1) && !rest.trim().is_empty()
        })
}

fn blocks(lines: &[(usize, &str)], names: &HashMap<String, String>) -> Vec<(usize, Block)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        let (number, line) = lines[at];
        if line.trim().is_empty() {
            at += 1;
            continue;
        }
        if let Some((mark, length, info)) = fence_of(line) {
            let lead = indent(line);
            let mut text = Vec::new();
            at += 1;
            while at < lines.len() {
                let inner = lines[at].1;
                at += 1;
                if fence_of(inner).is_some_and(|(closing, long, rest)| {
                    closing == mark && long >= length && rest.trim().is_empty()
                }) {
                    break;
                }
                text.push(outdent(inner, lead));
            }
            let language = info.split_whitespace().next().unwrap_or_default();
            out.push((
                number,
                Block::Code {
                    language: language.trim_matches(['{', '}', '.']).to_string(),
                    text: text.join("\n"),
                },
            ));
        } else if let Some((level, text)) = heading(line) {
            out.push((
                number,
                Block::Heading {
                    level,
                    text: inlines(text, names),
                },
            ));
            at += 1;
        } else if rule(line) {
            out.push((number, Block::Rule));
            at += 1;
        } else if indent(line) >= 4 {
            let mut text = Vec::new();
            while at < lines.len() && (indent(lines[at].1) >= 4 || lines[at].1.trim().is_empty()) {
                text.push(outdent(lines[at].1, 4));
                at += 1;
            }
            while text.last().is_some_and(|line| line.trim().is_empty()) {
                text.pop();
            }
            out.push((
                number,
                Block::Code {
                    language: String::new(),
                    text: text.join("\n"),
                },
            ));
        } else if line.trim_start().starts_with('>') {
            let mut inner = Vec::new();
            while at < lines.len() {
                let (number, line) = lines[at];
                let text = line.trim_start();
                match text.strip_prefix('>') {
                    Some(rest) => inner.push((number, rest.strip_prefix(' ').unwrap_or(rest))),
                    // A line of the paragraph the quote ends with may
                    // leave the mark out.
                    None if !text.is_empty()
                        && !begins(line)
                        && inner
                            .last()
                            .is_some_and(|(_, last)| !last.trim().is_empty()) =>
                    {
                        inner.push((number, line))
                    }
                    None => break,
                }
                at += 1;
            }
            out.push((number, Block::Quote(strip(blocks(&inner, names)))));
        } else if let Some((first, _, mark, _)) = item(line) {
            let mut items = Vec::new();
            while at < lines.len() {
                let Some((_, width, _, rest)) =
                    item(lines[at].1).filter(|(number, _, other, _)| {
                        *other == mark && number.is_some() == first.is_some()
                    })
                else {
                    break;
                };
                let mut inner = vec![(lines[at].0, rest)];
                at += 1;
                while at < lines.len() {
                    let (number, line) = lines[at];
                    let blank = line.trim().is_empty();
                    if !blank && indent(line) < width {
                        // The paragraph an item ends with may go on
                        // without being set in.
                        let lazy = !begins(line)
                            && item(line).is_none()
                            && inner
                                .last()
                                .is_some_and(|(_, last)| !last.trim().is_empty());
                        if !lazy {
                            break;
                        }
                    }
                    inner.push((number, outdent(line, width)));
                    at += 1;
                }
                // Empty lines after an item are between items, not in it.
                // The item's own line stays, empty or not.
                while inner.len() > 1
                    && inner.last().is_some_and(|(_, line)| line.trim().is_empty())
                {
                    inner.pop();
                    at -= 1;
                }
                let mut done = None;
                if let Some((_, text)) = inner.first_mut() {
                    for (state, mark) in [(false, "[ ] "), (true, "[x] "), (true, "[X] ")] {
                        if let Some(rest) = text.strip_prefix(mark) {
                            (done, *text) = (Some(state), rest);
                        }
                    }
                }
                items.push(Item {
                    done,
                    blocks: strip(blocks(&inner, names)),
                });
                // Items may have empty lines between them.
                let mut next = at;
                while next < lines.len() && lines[next].1.trim().is_empty() {
                    next += 1;
                }
                if next < lines.len() && item(lines[next].1).is_some() {
                    at = next;
                }
            }
            out.push((number, Block::List { first, items }));
        } else if html(line) {
            let mut text = Vec::new();
            while at < lines.len() && !lines[at].1.trim().is_empty() {
                text.push(lines[at].1);
                at += 1;
            }
            out.push((number, Block::Html(text.join("\n"))));
        } else if let Some(align) = lines
            .get(at + 1)
            .and_then(|(_, under)| alignments(under))
            .filter(|align| line.contains('|') && cells(line).len() == align.len())
        {
            let row = |line: &str| -> Vec<Vec<Inline>> {
                let mut row: Vec<Vec<Inline>> = cells(line)
                    .iter()
                    .map(|cell| inlines(cell, names))
                    .collect();
                row.resize(align.len(), Vec::new());
                row
            };
            let head = row(line);
            let mut rows = Vec::new();
            at += 2;
            while at < lines.len() && !lines[at].1.trim().is_empty() && !begins(lines[at].1) {
                rows.push(row(lines[at].1));
                at += 1;
            }
            out.push((number, Block::Table { head, align, rows }));
        } else {
            let mut text = vec![line.trim_start()];
            at += 1;
            let mut level = None;
            while at < lines.len() {
                let next = lines[at].1;
                let under = next.trim();
                // A line of `=` or `-` under a paragraph makes it a heading.
                if indent(next) < 4 && !under.is_empty() && under.chars().all(|c| c == '=') {
                    level = Some(1);
                } else if indent(next) < 4 && under.len() >= 2 && under.chars().all(|c| c == '-') {
                    level = Some(2);
                }
                if level.is_some() {
                    at += 1;
                    break;
                }
                if under.is_empty() || begins(next) {
                    break;
                }
                text.push(next.trim_start());
                at += 1;
            }
            let text = inlines(&text.join("\n"), names);
            out.push((
                number,
                match level {
                    Some(level) => Block::Heading { level, text },
                    None => Block::Paragraph(text),
                },
            ));
        }
    }
    out
}

fn strip(blocks: Vec<(usize, Block)>) -> Vec<Block> {
    blocks.into_iter().map(|(_, block)| block).collect()
}

const PUNCTUATION: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/// `&amp;` and its like, as the character it stands for.
fn entity(text: &str) -> Option<(char, usize)> {
    let end = text.find(';').filter(|end| *end <= 10)?;
    let name = &text[1..end];
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '\u{a9}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "hellip" => '\u{2026}',
        _ => {
            let code = name.strip_prefix('#')?;
            let code = match code.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => code.parse().ok()?,
            };
            char::from_u32(code).filter(|c| *c != '\0')?
        }
    };
    Some((c, end + 1))
}

/// Where the bracket that closes the one before `text` is.
fn closing(text: &str, open: char, close: char) -> Option<usize> {
    let (mut depth, mut escaped, mut code) = (0usize, false, false);
    for (at, c) in text.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '`' => code = !code,
            _ if code => {}
            c if c == open => depth += 1,
            c if c == close && depth == 0 => return Some(at),
            c if c == close => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The address in `(address "title")`, and how much of `text` that was.
fn destination(text: &str) -> Option<(String, usize)> {
    let inner = text.strip_prefix('(')?;
    let end = closing(inner, '(', ')')?;
    let address = inner[..end].trim();
    let address = match address.strip_prefix('<') {
        Some(rest) => rest.split('>').next()?,
        None => address.split_whitespace().next().unwrap_or_default(),
    };
    Some((address.replace('\\', ""), end + 2))
}

/// What is typed of `inlines` and nothing of how it is set: the words a
/// picture is described with.
pub fn plain(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(text) | Inline::Code(text) => out.push_str(text),
            Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                out.push_str(&plain(inner))
            }
            Inline::Link { text, .. } => out.push_str(&plain(text)),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::Break => out.push(' '),
        }
    }
    out
}

fn inlines(text: &str, names: &HashMap<String, String>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    let mut plain_text = String::new();
    let mut at = 0;
    macro_rules! flush {
        () => {
            if !plain_text.is_empty() {
                out.push(Inline::Text(std::mem::take(&mut plain_text)));
            }
        };
    }
    while at < text.len() {
        let rest = &text[at..];
        let c = rest.chars().next().unwrap();
        let before = text[..at].chars().next_back();
        match c {
            '\\' => {
                let next = rest[1..].chars().next();
                match next {
                    Some('\n') => {
                        flush!();
                        out.push(Inline::Break);
                        at += 2;
                    }
                    Some(next) if PUNCTUATION.contains(next) => {
                        plain_text.push(next);
                        at += 1 + next.len_utf8();
                    }
                    _ => {
                        plain_text.push('\\');
                        at += 1;
                    }
                }
                continue;
            }
            '\n' => {
                // Two spaces before the end of a line break it; without
                // them the next line goes on after a space.
                let hard = plain_text.ends_with("  ");
                let kept = plain_text.trim_end().len();
                plain_text.truncate(kept);
                if hard {
                    flush!();
                    out.push(Inline::Break);
                } else {
                    plain_text.push(' ');
                }
                at += 1;
                continue;
            }
            '&' => {
                if let Some((c, length)) = entity(rest) {
                    plain_text.push(c);
                    at += length;
                    continue;
                }
            }
            '`' => {
                let ticks = rest.chars().take_while(|c| *c == '`').count();
                let inner = &rest[ticks..];
                let mut search = 0;
                let mut found = None;
                while let Some(start) = inner[search..].find('`') {
                    let start = search + start;
                    let run = inner[start..].chars().take_while(|c| *c == '`').count();
                    if run == ticks {
                        found = Some(start);
                        break;
                    }
                    search = start + run;
                }
                if let Some(end) = found {
                    let code = inner[..end].replace('\n', " ");
                    let spaced = code.starts_with(' ') && code.ends_with(' ') && code.trim() != "";
                    let code = match spaced {
                        true => code[1..code.len() - 1].to_string(),
                        false => code,
                    };
                    flush!();
                    out.push(Inline::Code(code));
                    at += ticks + end + ticks;
                    continue;
                }
                plain_text.push_str(&rest[..ticks]);
                at += ticks;
                continue;
            }
            '!' if rest[1..].starts_with('[') => {
                if let Some(end) = closing(&rest[2..], '[', ']') {
                    let alt = plain(&inlines(&rest[2..2 + end], names));
                    let after = &rest[2 + end + 1..];
                    let named = || names.get(&label(&rest[2..2 + end])).cloned();
                    let found = destination(after)
                        .map(|(from, length)| (from, 2 + end + 1 + length))
                        .or_else(|| {
                            reference(after, names).map(|(from, n)| (from, 2 + end + 1 + n))
                        })
                        .or_else(|| named().map(|from| (from, 2 + end + 1)));
                    if let Some((from, length)) = found {
                        flush!();
                        out.push(Inline::Image { alt, from });
                        at += length;
                        continue;
                    }
                }
            }
            '[' => {
                if let Some(end) = closing(&rest[1..], '[', ']') {
                    let inner = &rest[1..1 + end];
                    let after = &rest[1 + end + 1..];
                    let found = destination(after)
                        .map(|(to, length)| (to, 1 + end + 1 + length))
                        .or_else(|| reference(after, names).map(|(to, n)| (to, 1 + end + 1 + n)))
                        .or_else(|| {
                            names
                                .get(&label(inner))
                                .cloned()
                                .map(|to| (to, 1 + end + 1))
                        });
                    if let Some((to, length)) = found {
                        flush!();
                        out.push(Inline::Link {
                            text: inlines(inner, names),
                            to,
                        });
                        at += length;
                        continue;
                    }
                }
            }
            '<' => {
                if let Some(end) = rest.find('>') {
                    let inner = &rest[1..end];
                    let tag = inner.trim_end_matches('/').trim().to_lowercase();
                    if tag == "br" {
                        flush!();
                        out.push(Inline::Break);
                        at += end + 1;
                        continue;
                    }
                    let address = !inner.contains(char::is_whitespace)
                        && (inner.contains("://") || inner.starts_with("mailto:"));
                    let mail = !inner.contains(char::is_whitespace)
                        && inner.split_once('@').is_some_and(|(name, host)| {
                            !name.is_empty() && host.contains('.') && !host.contains('/')
                        });
                    if address || mail {
                        flush!();
                        out.push(Inline::Link {
                            text: vec![Inline::Text(inner.to_string())],
                            to: match mail && !address {
                                true => format!("mailto:{inner}"),
                                false => inner.to_string(),
                            },
                        });
                        at += end + 1;
                        continue;
                    }
                }
            }
            'h' if (rest.starts_with("https://") || rest.starts_with("http://"))
                && before.is_none_or(|c| !c.is_alphanumeric()) =>
            {
                // An address written out is a link to itself, up to the
                // space after it and without the mark a sentence ends on.
                let end = rest
                    .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"'))
                    .unwrap_or(rest.len());
                let mut address = &rest[..end];
                while address.ends_with(['.', ',', ';', ':', '!', '?', '\'', '*', '_'])
                    || address.ends_with(')') && !address.contains('(')
                {
                    address = &address[..address.len() - 1];
                }
                if address.len() > "https://".len() {
                    flush!();
                    out.push(Inline::Link {
                        text: vec![Inline::Text(address.to_string())],
                        to: address.to_string(),
                    });
                    at += address.len();
                    continue;
                }
            }
            '*' | '_' | '~' => {
                let run = rest.chars().take_while(|d| *d == c).count();
                let after = rest[run..].chars().next();
                // A mark opens what follows it: not a space, and for an
                // underscore not the middle of a word.
                let opens = after.is_some_and(|c| !c.is_whitespace())
                    && (c != '_' || before.is_none_or(|c| !c.is_alphanumeric()))
                    && (c != '~' || run >= 2);
                if opens {
                    let wanted = if c == '~' { 2 } else { run.min(3) };
                    let mut found = None;
                    for size in (1..=wanted).rev() {
                        if c == '~' && size != 2 {
                            continue;
                        }
                        if let Some(end) = closer(&rest[size..], c, size) {
                            found = Some((size, end));
                            break;
                        }
                    }
                    if let Some((size, end)) = found {
                        let inner = inlines(&rest[size..size + end], names);
                        flush!();
                        out.push(match (c, size) {
                            ('~', _) => Inline::Strike(inner),
                            (_, 1) => Inline::Emphasis(inner),
                            (_, 2) => Inline::Strong(inner),
                            _ => Inline::Strong(vec![Inline::Emphasis(inner)]),
                        });
                        at += size + end + size;
                        continue;
                    }
                }
                plain_text.push_str(&rest[..run]);
                at += run;
                continue;
            }
            _ => {}
        }
        plain_text.push(c);
        at += c.len_utf8();
    }
    flush!();
    out
}

/// `[name]` after a link's text, as the address the name stands for.
fn reference(after: &str, names: &HashMap<String, String>) -> Option<(String, usize)> {
    let inner = after.strip_prefix('[')?;
    let end = inner.find(']')?;
    let to = names.get(&label(&inner[..end]))?;
    Some((to.clone(), end + 2))
}

/// Where the run of `size` marks that closes an emphasis is in `text`,
/// which follows the run that opened it.
fn closer(text: &str, mark: char, size: usize) -> Option<usize> {
    let (mut at, mut escaped, mut before) = (0, false, None::<char>);
    while at < text.len() {
        let c = text[at..].chars().next().unwrap();
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '`' {
            // Marks inside code are code.
            let ticks = text[at..].chars().take_while(|c| *c == '`').count();
            if let Some(end) = text[at + ticks..].find(&"`".repeat(ticks)) {
                at += ticks + end + ticks;
                before = Some('`');
                continue;
            }
        } else if c == mark {
            let run = text[at..].chars().take_while(|c| *c == mark).count();
            let after = text[at + run..].chars().next();
            let closes = before.is_some_and(|c| !c.is_whitespace())
                && (mark != '_' || after.is_none_or(|c| !c.is_alphanumeric()));
            if closes && run >= size && (run == size || run - size != size || size == 3) {
                // Of a longer run, the marks nearest the text are the
                // ones of something inside it.
                return Some(at + run - size);
            }
            at += run;
            before = Some(mark);
            continue;
        }
        before = Some(c);
        at += c.len_utf8();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Inline {
        Inline::Text(text.into())
    }

    fn only(source: &str) -> Vec<Block> {
        strip(parse(source))
    }

    fn line(source: &str) -> Vec<Inline> {
        match only(source).as_slice() {
            [Block::Paragraph(inlines)] => inlines.clone(),
            other => panic!("not one paragraph: {other:?}"),
        }
    }

    #[test]
    fn headings_paragraphs_and_rules_are_told_apart() {
        let source =
            "# One #\n\ntext that\ngoes on\n\nTwo\n---\n\n***\n#not a heading\n####### seven";
        assert_eq!(
            only(source),
            [
                Block::Heading {
                    level: 1,
                    text: vec![text("One")]
                },
                Block::Paragraph(vec![text("text that goes on")]),
                Block::Heading {
                    level: 2,
                    text: vec![text("Two")]
                },
                Block::Rule,
                Block::Paragraph(vec![text("#not a heading ####### seven")]),
            ]
        );
        // Each block says where it starts in the file.
        let lines: Vec<usize> = parse(source).iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [0, 2, 5, 8, 9]);
    }

    #[test]
    fn code_is_kept_as_written() {
        assert_eq!(
            only(
                "```rust title\nfn main() {\n    *x* [a](b)\n}\n```\nafter\n\n    set in\n\n    more\n\n~~~\n```\n~~~"
            ),
            [
                Block::Code {
                    language: "rust".into(),
                    text: "fn main() {\n    *x* [a](b)\n}".into()
                },
                Block::Paragraph(vec![text("after")]),
                Block::Code {
                    language: String::new(),
                    text: "set in\n\nmore".into()
                },
                Block::Code {
                    language: String::new(),
                    text: "```".into()
                },
            ]
        );
        // A fence nobody closed takes the rest of the file.
        assert_eq!(
            only("```\nopen\n# still code"),
            [Block::Code {
                language: String::new(),
                text: "open\n# still code".into()
            }]
        );
    }

    #[test]
    fn lists_nest_and_tasks_say_whether_they_are_done() {
        let paragraph = |words: &str| Block::Paragraph(vec![text(words)]);
        assert_eq!(
            only(
                "- one\n- [x] two\n  goes on\n\n  second paragraph\n  - inner\n- [ ] three\n\n3. third\n4. fourth\n"
            ),
            [
                Block::List {
                    first: None,
                    items: vec![
                        Item {
                            done: None,
                            blocks: vec![paragraph("one")]
                        },
                        Item {
                            done: Some(true),
                            blocks: vec![
                                paragraph("two goes on"),
                                paragraph("second paragraph"),
                                Block::List {
                                    first: None,
                                    items: vec![Item {
                                        done: None,
                                        blocks: vec![paragraph("inner")]
                                    }]
                                },
                            ]
                        },
                        Item {
                            done: Some(false),
                            blocks: vec![paragraph("three")]
                        },
                    ]
                },
                Block::List {
                    first: Some(3),
                    items: vec![
                        Item {
                            done: None,
                            blocks: vec![paragraph("third")]
                        },
                        Item {
                            done: None,
                            blocks: vec![paragraph("fourth")]
                        },
                    ]
                },
            ]
        );
        // A year at the start of a line is not an item of a list.
        assert_eq!(only("It was\n2024. Then"), [paragraph("It was 2024. Then")]);
    }

    #[test]
    fn quotes_and_tables_hold_what_is_in_them() {
        assert_eq!(
            only(
                "> # Said\n> a line\nthat goes on\n\n| Name | Count |\n|:-----|------:|\n| `a\\|b` | **2** |\n| c |\n\nafter"
            ),
            [
                Block::Quote(vec![
                    Block::Heading {
                        level: 1,
                        text: vec![text("Said")]
                    },
                    Block::Paragraph(vec![text("a line that goes on")]),
                ]),
                Block::Table {
                    head: vec![vec![text("Name")], vec![text("Count")]],
                    align: vec![Align::Left, Align::Right],
                    rows: vec![
                        vec![
                            vec![Inline::Code("a|b".into())],
                            vec![Inline::Strong(vec![text("2")])]
                        ],
                        vec![vec![text("c")], vec![]],
                    ],
                },
                Block::Paragraph(vec![text("after")]),
            ]
        );
        assert_eq!(
            only("<p align=\"center\">\n  <img src=\"a.png\">\n</p>\n\ntext"),
            [
                Block::Html("<p align=\"center\">\n  <img src=\"a.png\">\n</p>".into()),
                Block::Paragraph(vec![text("text")]),
            ]
        );
    }

    #[test]
    fn emphasis_code_and_breaks_in_a_line() {
        assert_eq!(
            line("a *b* **c** ***d*** ~~e~~ `f*g*` snake_case_name 2 * 3 * 4"),
            [
                text("a "),
                Inline::Emphasis(vec![text("b")]),
                text(" "),
                Inline::Strong(vec![text("c")]),
                text(" "),
                Inline::Strong(vec![Inline::Emphasis(vec![text("d")])]),
                text(" "),
                Inline::Strike(vec![text("e")]),
                text(" "),
                Inline::Code("f*g*".into()),
                text(" snake_case_name 2 * 3 * 4"),
            ]
        );
        assert_eq!(
            line("**bold with *slanted* inside** and _under_"),
            [
                Inline::Strong(vec![
                    text("bold with "),
                    Inline::Emphasis(vec![text("slanted")]),
                    text(" inside"),
                ]),
                text(" and "),
                Inline::Emphasis(vec![text("under")]),
            ]
        );
        assert_eq!(
            line("one  \ntwo\\\nthree<br>four\nfive \\* &amp; &#65; &nope;"),
            [
                text("one"),
                Inline::Break,
                text("two"),
                Inline::Break,
                text("three"),
                Inline::Break,
                text("four five * & A &nope;"),
            ]
        );
    }

    #[test]
    fn links_and_pictures_by_address_and_by_name() {
        let link = |words: &str, to: &str| Inline::Link {
            text: vec![text(words)],
            to: to.into(),
        };
        assert_eq!(
            line(
                "[a](https://a.example/x_(y) \"title\") [b][Home Page] [home page] [none] <https://c.example> https://d.example/e. <me@example.com>\n\n[home  page]: <https://b.example>"
            ),
            [
                link("a", "https://a.example/x_(y)"),
                text(" "),
                link("b", "https://b.example"),
                text(" "),
                link("home page", "https://b.example"),
                text(" [none] "),
                link("https://c.example", "https://c.example"),
                text(" "),
                link("https://d.example/e", "https://d.example/e"),
                text(". "),
                link("me@example.com", "mailto:me@example.com"),
            ]
        );
        assert_eq!(
            line(
                "![a *shot*](docs/shot.png) [![badge](b.svg)](https://ci.example) ![named][pic]\n\n[pic]: img/p.gif"
            ),
            [
                Inline::Image {
                    alt: "a shot".into(),
                    from: "docs/shot.png".into()
                },
                text(" "),
                Inline::Link {
                    text: vec![Inline::Image {
                        alt: "badge".into(),
                        from: "b.svg".into()
                    }],
                    to: "https://ci.example".into()
                },
                text(" "),
                Inline::Image {
                    alt: "named".into(),
                    from: "img/p.gif".into()
                },
            ]
        );
        // What names an address inside code is code.
        assert_eq!(
            only("```\n[pic]: not a name\n```"),
            [Block::Code {
                language: String::new(),
                text: "[pic]: not a name".into()
            }]
        );
    }

    #[test]
    fn nothing_typed_makes_it_fail() {
        for source in [
            "",
            "\n\n",
            "*",
            "**",
            "[",
            "![",
            "](",
            "`",
            "```",
            ">",
            "- ",
            "1.",
            "|",
            "|-|",
            "<",
            "&",
            "\\",
            "~~",
            "[a](",
            "[a][",
            "* * *",
            "_ _",
            "#",
            "\t- x\n\t\ty",
            "é*é*é",
            "🙂 **🙂** `🙂`",
        ] {
            let _ = parse(source);
        }
    }
}
