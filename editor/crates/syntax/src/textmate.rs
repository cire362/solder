//! TextMate grammars, the form VS Code extensions bring a language's colors
//! in: rules made of regular expressions, read a line at a time.
//!
//! A rule is one expression that colors what it matches, or a pair that
//! opens and closes a stretch with rules of its own inside, which may run
//! over many lines. What is open at the end of a line is all that the next
//! line needs to know, so a file is a list of lines, each with what it
//! leaves open and what it colors. A change is read again from the line it
//! is on until a line leaves open what it left before.
//!
//! The expressions are Oniguruma's. They are compiled by `fancy-regex`,
//! which reads most of that syntax; a rule whose expression it does not
//! read is left out, and the rest of the grammar colors what it can.
//!
//! There is no tree here: a language that has only such a grammar gets its
//! colors from it, and what it says about typing from its configuration.

use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::Instant,
};

use fancy_regex::{Regex, RegexBuilder};
use ropey::Rope;
use serde_json::{Map, Value};

use crate::HighlightKind;

type RuleId = usize;
/// The grammar's own patterns: what `$self` names.
const ROOT: RuleId = 0;
/// How many steps an expression may take on one line before it is given up
/// on there. Some grammars have expressions that never end on some lines.
const STEPS: usize = 100_000;
/// How deep rules may open inside each other on one line. Past it the line
/// is left as it is: a grammar that opens without end would use all memory.
const DEPTH: usize = 64;

struct Pattern {
    regex: Regex,
    /// It asks where the last match ended (`\G`), so where a match of it is
    /// depends on where the search starts.
    anchored: bool,
}

type Captures = Vec<(usize, HighlightKind)>;

enum Rule {
    Match {
        pattern: Option<Arc<Pattern>>,
        kind: Option<HighlightKind>,
        captures: Captures,
    },
    Span {
        begin: Option<Arc<Pattern>>,
        /// What closes it, or with `while_`, what each further line has to
        /// start with for it to go on. As written: it may name what the
        /// opening matched (`\1`), so it is compiled when the rule opens.
        end: String,
        while_: bool,
        /// Its own patterns are tried before what closes it.
        end_last: bool,
        /// The color of the whole stretch, and of what is inside the two
        /// ends if that differs.
        kind: Option<HighlightKind>,
        content: Option<HighlightKind>,
        begin_captures: Captures,
        end_captures: Captures,
        patterns: Vec<RuleId>,
    },
    List(Vec<RuleId>),
    /// The patterns of another grammar, by its scope, or one rule of them.
    External {
        scope: String,
        name: Option<String>,
    },
}

pub struct Grammar {
    rules: Vec<Rule>,
    /// The rules it names at the top, for another grammar that asks for
    /// one of them.
    names: HashMap<String, RuleId>,
    /// What a rule's patterns come to once the lists and the other grammars
    /// in them are followed: what is tried at a place in a line.
    flat: Mutex<HashMap<RuleId, Arc<Vec<Candidate>>>>,
}

type Candidate = (Arc<Grammar>, RuleId);

/// The color of a scope, or of the first of several that has one. Scopes
/// are dotted names, the general part first: `string.quoted.double.demo`.
pub fn kind_of(scopes: &str) -> Option<HighlightKind> {
    use HighlightKind::*;
    scopes.split_whitespace().find_map(|scope| {
        let has = |prefix: &str| {
            scope == prefix
                || scope
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
        };
        // The more exact names first: an escape in a string is not a string.
        Some(if has("comment") {
            Comment
        } else if has("constant.character.escape") || has("constant.other.placeholder") {
            Constant
        } else if has("string") || has("markup.inline.raw") || has("markup.raw") {
            String
        } else if has("constant.numeric") {
            Number
        } else if has("constant") || has("support.constant") {
            Constant
        } else if has("keyword.operator") {
            Operator
        } else if has("keyword") || has("storage") || has("markup.heading") {
            Keyword
        } else if has("entity.name.function")
            || has("support.function")
            || has("meta.function-call.generic")
        {
            Function
        } else if has("entity.name.tag") {
            Tag
        } else if has("entity.other.attribute-name") {
            Attribute
        } else if has("entity.name.type")
            || has("entity.name.class")
            || has("entity.name.namespace")
            || has("entity.other.inherited-class")
            || has("support.type")
            || has("support.class")
        {
            Type
        } else if has("variable.other.property") || has("meta.object-literal.key") {
            Property
        } else if has("variable") || has("support.variable") {
            Variable
        } else if has("punctuation") {
            Punctuation
        } else if has("entity.name") {
            Function
        } else {
            return None;
        })
    })
}

fn compile(source: &str) -> Option<Arc<Pattern>> {
    let regex = RegexBuilder::new(source)
        .oniguruma_mode(true)
        .backtrack_limit(STEPS)
        .build()
        .ok()?;
    Some(Arc::new(Pattern {
        regex,
        anchored: source.contains("\\G"),
    }))
}

/// Compiles what closes a rule, as it reads once it is open. The same few
/// come back line after line, so they are kept.
fn closing(source: &str) -> Option<Arc<Pattern>> {
    static KNOWN: Mutex<Option<HashMap<String, Option<Arc<Pattern>>>>> = Mutex::new(None);
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let known = known.get_or_insert_with(HashMap::new);
    if known.len() > 4096 {
        known.clear();
    }
    known
        .entry(source.to_string())
        .or_insert_with(|| compile(source))
        .clone()
}

struct Builder<'a> {
    rules: Vec<Rule>,
    /// The rules of the repositories met so far, by the repository they
    /// are in and their name there.
    known: HashMap<(usize, &'a str), RuleId>,
}

impl<'a> Builder<'a> {
    fn captures(value: &Value) -> Captures {
        value
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(index, capture)| {
                Some((index.parse().ok()?, kind_of(capture["name"].as_str()?)?))
            })
            .collect()
    }

    fn list(
        &mut self,
        patterns: &'a Value,
        repos: &mut Vec<&'a Map<String, Value>>,
    ) -> Vec<RuleId> {
        patterns
            .as_array()
            .into_iter()
            .flatten()
            .map(|pattern| self.rule(pattern, repos))
            .collect()
    }

    fn rule(&mut self, value: &'a Value, repos: &mut Vec<&'a Map<String, Value>>) -> RuleId {
        let id = self.rules.len();
        self.rules.push(Rule::List(Vec::new()));
        self.fill(id, value, repos);
        id
    }

    /// The rule a repository has under `name`: the innermost repository
    /// that has one. It gets its number before it is read, since rules
    /// name each other in circles.
    fn named(&mut self, name: &'a str, repos: &mut Vec<&'a Map<String, Value>>) -> Option<RuleId> {
        let depth = repos.iter().rposition(|repo| repo.contains_key(name))?;
        let repo = repos[depth];
        let key = (repo as *const Map<String, Value> as usize, name);
        if let Some(id) = self.known.get(&key) {
            return Some(*id);
        }
        let id = self.rules.len();
        self.rules.push(Rule::List(Vec::new()));
        self.known.insert(key, id);
        // What it names is looked for from its own repository outwards.
        let mut outer: Vec<&'a Map<String, Value>> = repos[..=depth].to_vec();
        self.fill(id, &repo[name], &mut outer);
        Some(id)
    }

    fn fill(&mut self, id: RuleId, value: &'a Value, repos: &mut Vec<&'a Map<String, Value>>) {
        let own = value["repository"].as_object();
        if let Some(own) = own {
            repos.push(own);
        }
        let name = value["name"].as_str().and_then(kind_of);
        let rule = if let Some(include) = value["include"].as_str() {
            match include {
                "$self" | "$base" => Rule::List(vec![ROOT]),
                local if local.starts_with('#') => {
                    Rule::List(self.named(&local[1..], repos).into_iter().collect())
                }
                other => {
                    let (scope, name) = match other.split_once('#') {
                        Some((scope, name)) => (scope, Some(name.to_string())),
                        None => (other, None),
                    };
                    Rule::External {
                        scope: scope.to_string(),
                        name,
                    }
                }
            }
        } else if let Some(begin) = value["begin"].as_str() {
            let both = Self::captures(&value["captures"]);
            let ends = |own: &Value| match own {
                Value::Null => both.clone(),
                own => Self::captures(own),
            };
            let (end, while_) = match (value["while"].as_str(), value["end"].as_str()) {
                (Some(source), _) => (source, true),
                // A rule that never closes runs to the end of the file.
                (None, end) => (end.unwrap_or("\\z(?!\\z)"), false),
            };
            Rule::Span {
                begin: compile(begin),
                end: end.to_string(),
                while_,
                end_last: value["applyEndPatternLast"] == 1 || value["applyEndPatternLast"] == true,
                kind: name,
                content: value["contentName"].as_str().and_then(kind_of),
                begin_captures: ends(&value["beginCaptures"]),
                end_captures: ends(&value["endCaptures"]),
                patterns: self.list(&value["patterns"], repos),
            }
        } else if let Some(source) = value["match"].as_str() {
            Rule::Match {
                pattern: compile(source),
                kind: name,
                captures: Self::captures(&value["captures"]),
            }
        } else {
            Rule::List(self.list(&value["patterns"], repos))
        };
        self.rules[id] = rule;
        if own.is_some() {
            repos.pop();
        }
    }
}

impl Grammar {
    /// Reads a grammar: JSON, or the property list TextMate itself wrote.
    pub fn parse(source: &str) -> Result<Grammar, String> {
        let value = if source.trim_start().starts_with('<') {
            import::plist::parse(source)?
        } else {
            import::jsonc::parse(source)?
        };
        if !value["patterns"].is_array() {
            return Err("It has no patterns".into());
        }
        let mut builder = Builder {
            rules: vec![Rule::List(Vec::new())],
            known: HashMap::new(),
        };
        let mut repos = Vec::new();
        let top = value["repository"].as_object();
        if let Some(top) = top {
            repos.push(top);
        }
        let root = builder.list(&value["patterns"], &mut repos);
        builder.rules[ROOT] = Rule::List(root);
        let mut names = HashMap::new();
        for name in top.into_iter().flat_map(|top| top.keys()) {
            if let Some(id) = builder.named(name, &mut repos) {
                names.insert(name.clone(), id);
            }
        }
        Ok(Grammar {
            rules: builder.rules,
            names,
            flat: Mutex::new(HashMap::new()),
        })
    }

    /// What is tried at a place inside `rule`: its patterns with the lists
    /// in them followed, and the grammars they bring in.
    fn candidates(self: &Arc<Self>, rule: RuleId) -> Arc<Vec<Candidate>> {
        if let Some(known) = self
            .flat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&rule)
        {
            return known.clone();
        }
        let ids: &[RuleId] = match &self.rules[rule] {
            Rule::Span { patterns, .. } | Rule::List(patterns) => patterns,
            _ => &[],
        };
        let mut found = Vec::new();
        let mut seen = Vec::new();
        for id in ids {
            self.follow(*id, &mut found, &mut seen);
        }
        let found = Arc::new(found);
        self.flat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(rule, found.clone());
        found
    }

    fn follow(
        self: &Arc<Self>,
        id: RuleId,
        found: &mut Vec<Candidate>,
        seen: &mut Vec<(usize, RuleId)>,
    ) {
        let here = (Arc::as_ptr(self) as usize, id);
        if seen.contains(&here) || seen.len() > 4096 {
            return;
        }
        seen.push(here);
        match &self.rules[id] {
            Rule::List(ids) => {
                for id in ids {
                    self.follow(*id, found, seen);
                }
            }
            Rule::External { scope, name } => {
                let Some(other) = by_scope(scope) else { return };
                let rule = match name {
                    Some(name) => other.names.get(name).copied(),
                    None => Some(ROOT),
                };
                if let Some(rule) = rule {
                    other.follow(rule, found, seen);
                }
            }
            Rule::Match { .. } | Rule::Span { .. } => found.push((self.clone(), id)),
        }
    }
}

/// A rule that is open at a place in the text.
#[derive(Clone)]
struct Frame {
    grammar: Arc<Grammar>,
    rule: RuleId,
    /// What closes it, as it reads now that it is open.
    end: Arc<str>,
    /// The color of what is inside it and of its two ends.
    inside: Option<HighlightKind>,
    whole: Option<HighlightKind>,
}

impl PartialEq for Frame {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.grammar, &other.grammar)
            && self.rule == other.rule
            && self.end == other.end
    }
}

type Stack = Vec<Frame>;

/// Where an expression matched in a line: the whole match and its groups.
type Found = Vec<Option<(usize, usize)>>;

fn search(pattern: &Pattern, line: &str, from: usize) -> Option<Found> {
    let captures = pattern.regex.captures_from_pos(line, from).ok()??;
    Some(
        (0..captures.len())
            .map(|group| captures.get(group).map(|m| (m.start(), m.end())))
            .collect(),
    )
}

fn paint(slots: &mut [Option<HighlightKind>], range: (usize, usize), kind: Option<HighlightKind>) {
    if let (Some(kind), Some(slots)) = (kind, slots.get_mut(range.0..range.1)) {
        slots.fill(Some(kind));
    }
}

fn paint_groups(slots: &mut [Option<HighlightKind>], found: &Found, captures: &Captures) {
    for (group, kind) in captures {
        if let Some(Some(range)) = found.get(*group) {
            paint(slots, *range, Some(*kind));
        }
    }
}

/// What closes a rule once it is open: `\1` in it is what the opening's
/// first group matched, as text.
fn resolved(end: &str, line: &str, found: &Found) -> Arc<str> {
    if !end.contains('\\') {
        return end.into();
    }
    let mut out = String::with_capacity(end.len());
    let mut chars = end.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek().and_then(|next| next.to_digit(10))) {
            ('\\', Some(group)) => {
                chars.next();
                if let Some(Some((start, end))) = found.get(group as usize) {
                    out.push_str(&fancy_regex::escape(&line[*start..*end]));
                }
            }
            // An escaped backslash is not the start of a group's number.
            ('\\', None) => {
                out.push(c);
                out.extend(chars.next());
            }
            _ => out.push(c),
        }
    }
    out.into()
}

/// Reads one line: colors it into `slots`, one for each of its bytes, and
/// leaves in `stack` what is open at its end.
fn read_line(
    root: &Arc<Grammar>,
    line: &str,
    stack: &mut Stack,
    slots: &mut [Option<HighlightKind>],
) {
    // A rule that goes on only while lines start a certain way ends, with
    // all inside it, at the first line that does not.
    let mut pos = 0;
    let mut kept = 0;
    while kept < stack.len() {
        let frame = &stack[kept];
        let goes_on = match &frame.grammar.rules[frame.rule] {
            Rule::Span { while_: true, .. } => closing(&frame.end)
                .and_then(|pattern| search(&pattern, line, pos))
                .and_then(|found| found[0])
                .filter(|(start, _)| *start == pos)
                .map(|(_, end)| end),
            _ => Some(pos),
        };
        match goes_on {
            Some(end) => {
                paint(slots, (pos, end), frame.whole);
                pos = end;
                kept += 1;
            }
            None => stack.truncate(kept),
        }
    }

    // What each thing tried here found the last time it was looked for:
    // a match further on is still the next one when the place moves up.
    let mut cache: Vec<Option<Option<Found>>> = Vec::new();
    let mut tried: Arc<Vec<Candidate>> = Arc::new(Vec::new());
    let mut context: Option<(usize, RuleId, usize)> = None;
    let mut stuck = 0;
    let mut steps = line.len() * 4 + 64;
    loop {
        steps -= 1;
        if pos > line.len() || steps == 0 {
            break;
        }
        let top = stack.last().cloned();
        let (grammar, rule) = match &top {
            Some(frame) => (frame.grammar.clone(), frame.rule),
            None => (root.clone(), ROOT),
        };
        let now = (Arc::as_ptr(&grammar) as usize, rule, stack.len());
        if context != Some(now) {
            context = Some(now);
            tried = grammar.candidates(rule);
            cache = vec![None; tried.len()];
        }
        let inside = top.as_ref().and_then(|frame| frame.inside);
        // What closes the rule that is open, unless it only goes on by
        // how lines start.
        let (close, last) = match top.as_ref().map(|f| &f.grammar.rules[f.rule]) {
            Some(Rule::Span {
                while_: false,
                end_last,
                ..
            }) => (
                top.as_ref().and_then(|frame| closing(&frame.end)),
                *end_last,
            ),
            _ => (None, false),
        };
        let closes = close
            .as_ref()
            .and_then(|pattern| search(pattern, line, pos));
        let mut best: Option<(usize, usize, Found)> = None;
        for (index, (grammar, id)) in tried.iter().enumerate() {
            let pattern = match &grammar.rules[*id] {
                Rule::Match { pattern, .. } => pattern,
                Rule::Span { begin, .. } => begin,
                _ => continue,
            };
            let Some(pattern) = pattern else { continue };
            let fresh = match &cache[index] {
                Some(Some(found)) if !pattern.anchored => found[0].is_none_or(|(s, _)| s < pos),
                Some(None) if !pattern.anchored => false,
                _ => true,
            };
            if fresh {
                cache[index] = Some(search(pattern, line, pos));
            }
            let Some(Some(found)) = &cache[index] else {
                continue;
            };
            let Some((start, _)) = found[0] else { continue };
            if best.as_ref().is_none_or(|(known, _, _)| start < *known) {
                best = Some((start, index, found.clone()));
            }
        }
        // The nearest wins, and of two at one place what closes the rule,
        // unless the rule says its own patterns go first.
        let closing_first = match (&closes, &best) {
            (Some(closes), Some((start, _, _))) => {
                let at = closes[0].map_or(usize::MAX, |(s, _)| s);
                at < *start || (at == *start && !last)
            }
            (Some(_), None) => true,
            (None, _) => false,
        };
        if closing_first {
            let found = closes.unwrap_or_default();
            let Some((start, end)) = found[0] else { break };
            let frame = stack.pop();
            paint(slots, (pos, start), inside);
            if let Some(frame) = frame {
                paint(slots, (start, end), frame.whole);
                if let Rule::Span { end_captures, .. } = &frame.grammar.rules[frame.rule] {
                    paint_groups(slots, &found, end_captures);
                }
            }
            // Closed without taking anything: the rule that opened here
            // must not open again at the same place for ever.
            stuck = if end == pos { stuck + 1 } else { 0 };
            pos = end;
        } else if let Some((start, index, found)) = best {
            let Some((_, end)) = found[0] else { break };
            let (grammar, id) = &tried[index];
            paint(slots, (pos, start), inside);
            match &grammar.rules[*id] {
                Rule::Match { kind, captures, .. } => {
                    paint(slots, (start, end), kind.or(inside));
                    paint_groups(slots, &found, captures);
                    stuck = if end == pos { stuck + 1 } else { 0 };
                }
                Rule::Span {
                    end: closes,
                    kind,
                    content,
                    begin_captures,
                    ..
                } => {
                    let whole = kind.or(inside);
                    paint(slots, (start, end), whole);
                    paint_groups(slots, &found, begin_captures);
                    stuck = if end == pos { stuck + 1 } else { 0 };
                    if stack.len() < DEPTH {
                        stack.push(Frame {
                            grammar: grammar.clone(),
                            rule: *id,
                            end: resolved(closes, line, &found),
                            inside: content.or(whole),
                            whole,
                        });
                    }
                }
                _ => {}
            }
            pos = end;
        } else {
            paint(slots, (pos, line.len()), inside);
            break;
        }
        // Nothing was taken several times over at one place: step past it.
        if stuck > 2 {
            stuck = 0;
            let step = line[pos.min(line.len())..]
                .chars()
                .next()
                .map_or(1, char::len_utf8);
            paint(slots, (pos, pos + step), inside);
            pos += step;
            cache.iter_mut().for_each(|found| *found = None);
        }
    }
}

type Span = (u32, u32, HighlightKind);

#[derive(Clone)]
struct LineInfo {
    /// What is open at the end of the line.
    open: Arc<Stack>,
    /// What it colors, from its own start.
    spans: Arc<[Span]>,
}

/// A file as a grammar reads it: every line with its colors.
#[derive(Clone)]
pub struct Lines {
    grammar: Arc<Grammar>,
    lines: Arc<Vec<LineInfo>>,
    /// What changed since the lines were read: the first row that did,
    /// and how many rows at the end did not.
    changed: Option<(usize, usize)>,
    /// How many rows the text has now, as far as the changes say.
    rows: usize,
}

impl Lines {
    pub fn parse(grammar: Arc<Grammar>, rope: &Rope, deadline: Option<Instant>) -> Option<Lines> {
        let empty = Lines {
            grammar,
            lines: Arc::new(Vec::new()),
            changed: Some((0, 0)),
            rows: 0,
        };
        empty.reparsed(rope, deadline)
    }

    /// Notes a change: rows `start` to `old_end` became rows `start` to
    /// `new_end`.
    pub fn edit(&mut self, start: usize, old_end: usize, new_end: usize) {
        let after = self.rows.saturating_sub(old_end + 1);
        self.changed = Some(match self.changed {
            Some((first, kept)) => (first.min(start), kept.min(after)),
            None => (start, after),
        });
        self.rows = self.rows + new_end - old_end.min(self.rows + new_end);
    }

    /// The lines of the text as it is now. Read again from the first row
    /// that changed, until a row past the change leaves open what the row
    /// that was there left: the rest is as it was. `None` if `deadline`
    /// passes first.
    pub fn reparsed(&self, rope: &Rope, deadline: Option<Instant>) -> Option<Lines> {
        let total = rope.len_lines();
        let old = &self.lines;
        let Some((first, kept)) = self.changed else {
            return Some(self.clone());
        };
        let first = first.min(old.len()).min(total);
        // The rows at the end that did not change, in both texts.
        let kept = kept.min(old.len() - first).min(total - first);
        let mut lines: Vec<LineInfo> = old[..first].to_vec();
        let mut stack: Stack = match first.checked_sub(1) {
            Some(above) => (*old[above].open).clone(),
            None => Vec::new(),
        };
        let mut slots: Vec<Option<HighlightKind>> = Vec::new();
        let mut text = String::new();
        for row in first..total {
            // Past the change, a row that starts as the old one did reads
            // as the old one did, and so does every row after it.
            if row >= total - kept {
                let was = row + old.len() - total;
                if was >= 1 && *old[was - 1].open == stack {
                    lines.extend_from_slice(&old[was..]);
                    break;
                }
            }
            if row % 64 == 0 && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return None;
            }
            text.clear();
            text.extend(rope.line(row).chunks());
            // The end of the line is not part of what rules match.
            let line = text.trim_end_matches(['\n', '\r']);
            slots.clear();
            slots.resize(line.len(), None);
            read_line(&self.grammar, line, &mut stack, &mut slots);
            let mut spans: Vec<Span> = Vec::new();
            let mut start = 0;
            for end in 1..=slots.len() {
                if end == slots.len() || slots[end] != slots[start] {
                    if let Some(kind) = slots[start] {
                        spans.push((start as u32, end as u32, kind));
                    }
                    start = end;
                }
            }
            // A row that leaves open what the one above left shares it.
            let open = match lines.last() {
                Some(above) if *above.open == stack => above.open.clone(),
                _ => Arc::new(stack.clone()),
            };
            lines.push(LineInfo {
                open,
                spans: spans.into(),
            });
        }
        Some(Lines {
            grammar: self.grammar.clone(),
            rows: lines.len(),
            lines: Arc::new(lines),
            changed: None,
        })
    }

    /// The colors of the bytes in `range`, in order.
    pub fn highlights(
        &self,
        rope: &Rope,
        range: Range<usize>,
        out: &mut Vec<(Range<usize>, HighlightKind)>,
    ) {
        let len = rope.len_bytes();
        let (from, to) = (range.start.min(len), range.end.min(len));
        if self.lines.is_empty() || from >= to {
            return;
        }
        let last = rope.byte_to_line(to).min(self.lines.len() - 1);
        for row in rope.byte_to_line(from)..=last {
            let base = rope.line_to_byte(row);
            for (start, end, kind) in self.lines[row].spans.iter() {
                let (start, end) = (base + *start as usize, base + *end as usize);
                if end > from && start < to {
                    out.push((start.max(from)..end.min(to), *kind));
                }
            }
        }
    }
}

/// Where the grammar of each scope is, among all that installed
/// extensions bring: a grammar may ask for another by its scope.
fn scopes() -> &'static RwLock<HashMap<String, PathBuf>> {
    static SCOPES: OnceLock<RwLock<HashMap<String, PathBuf>>> = OnceLock::new();
    SCOPES.get_or_init(Default::default)
}

/// Says where the grammars are, by scope. Replaces what was said before.
pub fn set_grammars(by_scope: HashMap<String, PathBuf>) {
    *scopes().write().unwrap_or_else(|e| e.into_inner()) = by_scope;
    // A grammar read with the old set may have followed another that is
    // no longer there; all are read again when next asked for.
    loaded().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

fn loaded() -> &'static Mutex<HashMap<PathBuf, Option<Arc<Grammar>>>> {
    static LOADED: OnceLock<Mutex<HashMap<PathBuf, Option<Arc<Grammar>>>>> = OnceLock::new();
    LOADED.get_or_init(Default::default)
}

/// The grammar in the file at `path`, read once. Slow the first time: it
/// reads the file and compiles every expression in it.
pub fn load(path: &Path) -> Option<Arc<Grammar>> {
    if let Some(known) = loaded().lock().unwrap_or_else(|e| e.into_inner()).get(path) {
        return known.clone();
    }
    let grammar = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|source| Grammar::parse(&source))
        .map_err(|e| eprintln!("grammar {}: {e}", path.display()))
        .ok()
        .map(Arc::new);
    loaded()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_path_buf(), grammar.clone());
    grammar
}

fn by_scope(scope: &str) -> Option<Arc<Grammar>> {
    let path = scopes()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(scope)
        .cloned()?;
    load(&path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use HighlightKind::*;

    const DEMO: &str = r##"{
      // A small language: comments, strings, words, and blocks in braces.
      "scopeName": "source.demo",
      "patterns": [{ "include": "#all" }],
      "repository": {
        "all": { "patterns": [
          { "name": "comment.line.demo", "match": "#.*$" },
          { "name": "comment.block.demo", "begin": "/\\*", "end": "\\*/" },
          { "include": "#string" },
          { "name": "keyword.control.demo", "match": "\\b(if|else|let)\\b" },
          { "name": "constant.numeric.demo", "match": "\\b\\d+\\b" },
          { "match": "\\b(fn)\\s+(\\w+)", "captures": {
              "1": { "name": "storage.type.demo" }, "2": { "name": "entity.name.function.demo" } } },
          { "begin": "<<(\\w+)", "end": "^\\1$", "contentName": "string.unquoted.heredoc.demo",
            "beginCaptures": { "0": { "name": "keyword.operator.demo" } } },
          { "begin": "\\{", "end": "\\}", "beginCaptures": { "0": { "name": "punctuation.demo" } },
            "endCaptures": { "0": { "name": "punctuation.demo" } },
            "patterns": [{ "include": "$self" }] },
          { "begin": "^>", "while": "^>", "name": "markup.raw.demo" },
          { "match": "(?<!\\w)@\\w+", "name": "entity.other.attribute-name.demo" },
          { "match": "a(?~bc)", "name": "invalid.demo" }
        ] },
        "string": { "name": "string.quoted.double.demo", "begin": "\"", "end": "\"",
          "patterns": [{ "name": "constant.character.escape.demo", "match": "\\\\." }] }
      }
    }"##;

    type Colored = (std::string::String, HighlightKind);

    fn colors(lines: &Lines, source: &str) -> Vec<Colored> {
        let rope = Rope::from_str(source);
        let mut out = Vec::new();
        lines.highlights(&rope, 0..source.len(), &mut out);
        out.into_iter()
            .map(|(range, kind)| (source[range].to_string(), kind))
            .collect()
    }

    fn read(source: &str) -> Lines {
        let grammar = Arc::new(Grammar::parse(DEMO).unwrap());
        Lines::parse(grammar, &Rope::from_str(source), None).unwrap()
    }

    fn pair(text: &str, kind: HighlightKind) -> Colored {
        (text.to_string(), kind)
    }

    #[test]
    fn scopes_have_the_colors_of_what_they_name() {
        assert_eq!(kind_of("comment.line.double-slash.js"), Some(Comment));
        assert_eq!(kind_of("string.quoted.double"), Some(String));
        assert_eq!(kind_of("constant.character.escape.js"), Some(Constant));
        assert_eq!(kind_of("constant.numeric.hex"), Some(Number));
        assert_eq!(kind_of("keyword.operator.assignment"), Some(Operator));
        assert_eq!(kind_of("keyword.control.flow"), Some(Keyword));
        assert_eq!(kind_of("storage.type.function"), Some(Keyword));
        assert_eq!(kind_of("entity.name.function.js"), Some(Function));
        assert_eq!(kind_of("entity.name.type.class"), Some(Type));
        assert_eq!(kind_of("entity.name.tag.html"), Some(Tag));
        assert_eq!(kind_of("entity.other.attribute-name"), Some(Attribute));
        assert_eq!(kind_of("variable.parameter"), Some(Variable));
        assert_eq!(
            kind_of("punctuation.definition.string.begin"),
            Some(Punctuation)
        );
        // A name that only starts like one is not it, and the first of
        // several that has a color gives it.
        assert_eq!(kind_of("commentary"), None);
        assert_eq!(kind_of("meta.block source.demo"), None);
        assert_eq!(kind_of("meta.block string.quoted"), Some(String));
    }

    #[test]
    fn a_grammar_colors_what_its_rules_match() {
        let source = "let x = 42 # note\nfn add \"a\\\"b\" @tag e@mail\n";
        assert_eq!(
            colors(&read(source), source),
            [
                pair("let", Keyword),
                pair("42", Number),
                pair("# note", Comment),
                // The groups of one match, each with its own color.
                pair("fn", Keyword),
                pair("add", Function),
                // A rule inside a rule: the escape in the string.
                pair("\"a", String),
                pair("\\\"", Constant),
                pair("b\"", String),
                // Looking behind: `@` after a word is not a tag.
                pair("@tag", Attribute),
            ]
        );
        // An expression this does not read leaves its rule out and the
        // others in: the last rule of the grammar is one.
        assert_eq!(colors(&read("abc 7"), "abc 7"), [pair("7", Number)]);
    }

    #[test]
    fn what_is_open_at_the_end_of_a_line_goes_on_in_the_next() {
        let source = "1 /* two\nthree 3 */ 4\n{ if {\n 5 } }\n";
        assert_eq!(
            colors(&read(source), source),
            [
                pair("1", Number),
                pair("/* two", Comment),
                pair("three 3 */", Comment),
                pair("4", Number),
                // The grammar's own rules inside its braces, to any depth.
                pair("{", Punctuation),
                pair("if", Keyword),
                pair("{", Punctuation),
                pair("5", Number),
                pair("}", Punctuation),
                pair("}", Punctuation),
            ]
        );
        // What closes a rule may be what opened it: the word after `<<`.
        let source = "<<END\nlet 1\nEND\nlet\n";
        assert_eq!(
            colors(&read(source), source),
            [
                pair("<<END", Operator),
                pair("let 1", String),
                pair("let", Keyword),
            ]
        );
        // A rule that goes on while lines start a certain way.
        let source = "> a 1\n> b\nc 2\n";
        assert_eq!(
            colors(&read(source), source),
            [
                pair("> a 1", String),
                pair("> b", String),
                pair("2", Number),
            ]
        );
    }

    #[test]
    fn a_change_is_read_again_only_as_far_as_it_reaches() {
        let before = "let 1\nlet 2\nlet 3\nlet 4\nlet 5\n";
        let mut lines = read(before);
        // A comment is opened on the second row and closed on the third:
        // those two are read again, and the rows after them are not.
        let after = "let 1\nlet /* 2\nlet 3 */\nlet 4\nlet 5\n";
        lines.edit(1, 2, 2);
        let fresh = lines.reparsed(&Rope::from_str(after), None).unwrap();
        assert_eq!(
            colors(&fresh, after)[..7],
            [
                pair("let", Keyword),
                pair("1", Number),
                pair("let", Keyword),
                pair("/* 2", Comment),
                pair("let 3 */", Comment),
                pair("let", Keyword),
                pair("4", Number),
            ]
        );
        assert!(Arc::ptr_eq(&lines.lines[3].spans, &fresh.lines[3].spans));
        assert!(Arc::ptr_eq(&lines.lines[4].spans, &fresh.lines[4].spans));
        assert!(!Arc::ptr_eq(&lines.lines[2].spans, &fresh.lines[2].spans));
        // Left open instead, the comment reaches the end of the file:
        // every row after it is read again, since each starts otherwise.
        let open = "let 1\nlet /* 2\nlet 3\nlet 4\nlet 5\n";
        let mut unclosed = read(before);
        unclosed.edit(1, 1, 1);
        let unclosed = unclosed.reparsed(&Rope::from_str(open), None).unwrap();
        assert_eq!(
            colors(&unclosed, open).last(),
            Some(&pair("let 5", Comment))
        );

        // Rows put in and taken out: the rows after them move, and are
        // still the ones that were there.
        let mut lines = fresh;
        let longer = "let 1\nlet /* 2\nlet 3 */\nnew 9\nnew 8\nlet 4\nlet 5\n";
        lines.edit(3, 3, 5);
        let more = lines.reparsed(&Rope::from_str(longer), None).unwrap();
        assert_eq!(more.lines.len(), lines.lines.len() + 2);
        assert!(Arc::ptr_eq(&lines.lines[4].spans, &more.lines[6].spans));
        assert_eq!(colors(&more, longer)[5], pair("9", Number));
        let mut lines = more;
        let shorter = "let 1\nlet 4\nlet 5\n";
        lines.edit(1, 5, 1);
        let fewer = lines.reparsed(&Rope::from_str(shorter), None).unwrap();
        assert!(Arc::ptr_eq(&lines.lines[6].spans, &fewer.lines[2].spans));
        assert_eq!(
            colors(&fewer, shorter)[..4],
            [
                pair("let", Keyword),
                pair("1", Number),
                pair("let", Keyword),
                pair("4", Number)
            ]
        );
        // And the same as reading the whole text anew, each time.
        assert_eq!(colors(&fewer, shorter), colors(&read(shorter), shorter));
        // No time at all is not enough time.
        let late = Some(Instant::now() - std::time::Duration::from_secs(1));
        let grammar = Arc::new(Grammar::parse(DEMO).unwrap());
        assert!(Lines::parse(grammar, &Rope::from_str(before), late).is_none());
    }

    #[test]
    fn a_grammar_in_a_property_list_and_one_that_is_none() {
        let plist = r#"<?xml version="1.0"?>
<plist version="1.0"><dict>
  <key>scopeName</key><string>source.old</string>
  <key>patterns</key><array>
    <dict><key>name</key><string>comment.line.old</string><key>match</key><string>;.*$</string></dict>
  </array>
</dict></plist>"#;
        let grammar = Arc::new(Grammar::parse(plist).unwrap());
        let source = "word ; rest\n";
        let lines = Lines::parse(grammar, &Rope::from_str(source), None).unwrap();
        assert_eq!(colors(&lines, source), [pair("; rest", Comment)]);
        assert!(Grammar::parse("{ \"name\": \"no patterns\" }").is_err());
        assert!(Grammar::parse("not a grammar").is_err());
    }
}
