//! Query options shared by the find bar and project search.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

impl SearchOptions {
    /// The pattern handed to the regex engine.
    pub fn pattern(&self, query: &str) -> String {
        let pattern = if self.regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        if self.whole_word {
            format!(r"\b(?:{pattern})\b")
        } else {
            pattern
        }
    }

    pub fn build(&self, query: &str) -> Result<Regex, String> {
        RegexBuilder::new(&self.pattern(query))
            .case_insensitive(!self.case_sensitive)
            .multi_line(true)
            .size_limit(1 << 24)
            .build()
            .map_err(|e| match e {
                regex::Error::Syntax(s) => {
                    s.lines().last().unwrap_or("Invalid pattern").to_string()
                }
                other => other.to_string(),
            })
    }
}

/// Non-empty matches in `text`, at most `limit` of them.
pub fn find_all(text: &str, re: &Regex, limit: usize) -> Vec<Range<usize>> {
    re.find_iter(text)
        .filter(|m| !m.is_empty())
        .map(|m| m.range())
        .take(limit)
        .collect()
}

/// The text that replaces `matched`. In regex mode `$1` and `${name}` expand.
pub fn replacement_for(
    re: &Regex,
    matched: &str,
    replacement: &str,
    options: SearchOptions,
) -> String {
    if !options.regex {
        return replacement.to_string();
    }
    match re.captures(matched) {
        Some(caps) => {
            let mut out = String::new();
            caps.expand(replacement, &mut out);
            out
        }
        None => replacement.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_word_and_case() {
        let text = "Foo foo food a.b";
        let o = SearchOptions::default();
        assert_eq!(
            find_all(text, &o.build("foo").unwrap(), 100),
            vec![0..3, 4..7, 8..11]
        );
        let o = SearchOptions {
            whole_word: true,
            ..o
        };
        assert_eq!(
            find_all(text, &o.build("foo").unwrap(), 100),
            vec![0..3, 4..7]
        );
        let o = SearchOptions {
            case_sensitive: true,
            whole_word: false,
            regex: false,
        };
        assert_eq!(
            find_all(text, &o.build("foo").unwrap(), 100),
            vec![4..7, 8..11]
        );
        assert_eq!(
            find_all(text, &SearchOptions::default().build("a.b").unwrap(), 9),
            vec![13..16]
        );
    }

    #[test]
    fn regex_replacement_expands_groups() {
        let o = SearchOptions {
            regex: true,
            ..Default::default()
        };
        let re = o.build(r"(\w+)@(\w+)").unwrap();
        assert_eq!(replacement_for(&re, "me@host", "$2:$1", o), "host:me");
        assert!(o.build("(").is_err());
    }
}
