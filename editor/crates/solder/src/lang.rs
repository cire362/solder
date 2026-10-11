//! The language the interface speaks: English as it is written in the
//! code, or Russian from a table.
//!
//! A string the user reads is written in English where it is used and
//! passed through `t`, or `tf` if it has parts put into it. English is
//! then the key, so a string that has no Russian yet reads in English
//! and nothing is ever missing. `lang_ru.rs` is the table, and a test
//! holds it to the code: every string given to `t` and every command of
//! the palette has an entry, and no entry is of a string that is gone.

use std::{collections::HashMap, fmt::Display, sync::OnceLock};

use serde::{Deserialize, Serialize};

#[path = "lang_ru.rs"]
mod ru;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "ru")]
    Russian,
}

// Tests run side by side, each on a thread of its own with its own
// interface: one that reads Russian must not turn the others Russian.
#[cfg(test)]
thread_local! {
    static CURRENT: std::cell::Cell<Language> = const { std::cell::Cell::new(Language::English) };
}
#[cfg(not(test))]
static CURRENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Puts a language in use. What is drawn after this reads in it.
pub fn set(language: Language) {
    #[cfg(test)]
    CURRENT.with(|current| current.set(language));
    #[cfg(not(test))]
    CURRENT.store(language as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn current() -> Language {
    #[cfg(test)]
    return CURRENT.with(|current| current.get());
    #[cfg(not(test))]
    match CURRENT.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Language::Russian,
        _ => Language::English,
    }
}

fn russian() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| ru::WORDS.iter().chain(ru::COMMANDS).copied().collect())
}

/// `text` in the language in use.
pub fn t(text: &'static str) -> &'static str {
    match current() {
        Language::English => text,
        Language::Russian => russian().get(text).copied().unwrap_or(text),
    }
}

/// `template` in the language in use, with each `{}` of it replaced by
/// the next of `with`. The Russian may name them in another order as
/// `{0}`, `{1}`.
pub fn tf(template: &'static str, with: &[&dyn Display]) -> String {
    let template = t(template);
    let mut out = String::with_capacity(template.len() + 16);
    let (mut rest, mut next) = (template, 0);
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        let Some(end) = rest[at..].find('}') else {
            rest = &rest[at..];
            break;
        };
        let name = &rest[at + 1..at + end];
        let which = match name.parse::<usize>() {
            Ok(which) => which,
            Err(_) => {
                next += 1;
                next - 1
            }
        };
        match with.get(which) {
            Some(part) => out.push_str(&part.to_string()),
            None => out.push_str(&rest[at..at + end + 1]),
        }
        rest = &rest[at + end + 1..];
    }
    out.push_str(rest);
    out
}

/// The name of a command of the palette in the language in use, from its
/// name in English as the palette makes it (`Editor: Move line up`).
pub fn command(name: &str) -> Option<&'static str> {
    match current() {
        Language::English => None,
        Language::Russian => russian().get(name).copied(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_reads_in_the_language_in_use() {
        assert_eq!(t("Open a folder"), "Open a folder");
        assert_eq!(
            tf("Go to line {}, column {}", &[&3, &7]),
            "Go to line 3, column 7"
        );
        assert_eq!(command("Workspace: Open folder"), None);
        set(Language::Russian);
        assert_eq!(t("Open a folder"), "Открыть папку");
        assert_eq!(
            tf("Go to line {}, column {}", &[&3, &7]),
            "Перейти к строке 3, столбец 7"
        );
        assert_eq!(
            command("Workspace: Open folder"),
            Some("Окно: открыть папку")
        );
        // What has no Russian reads as it is written.
        assert_eq!(t("No such words anywhere"), "No such words anywhere");
        assert_eq!(tf("{} and {} and {}", &[&1, &2]), "1 and 2 and {}");
        set(Language::English);
        assert_eq!(t("Open a folder"), "Open a folder");
    }

    #[test]
    fn parts_may_change_places_in_another_language() {
        assert_eq!(tf("{1} of {0}", &[&"a", &"b"]), "b of a");
        assert_eq!(tf("{} of {0}, {}", &[&"a", &"b"]), "a of a, b");
        assert_eq!(tf("an open { brace", &[&"a"]), "an open { brace");
    }

    /// Every string literal given to `t` or `tf` in the sources.
    fn used() -> Vec<String> {
        let mut found = Vec::new();
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut folders = vec![src];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(folder).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                    continue;
                }
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if !name.ends_with(".rs") || name == "lang.rs" || name == "lang_ru.rs" {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for call in ["t(", "tf("] {
                    let mut rest = text.as_str();
                    while let Some(at) = rest.find(call) {
                        let before = rest[..at].chars().next_back();
                        rest = &rest[at + call.len()..];
                        // `print(`, `assert(` and their like end in a `t`
                        // too, and not every call is of a literal. The
                        // literal may be on the line after the bracket.
                        let Some(after) = rest.trim_start().strip_prefix('"') else {
                            continue;
                        };
                        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                            continue;
                        }
                        rest = after;
                        let mut literal = String::new();
                        let mut chars = rest.chars();
                        while let Some(c) = chars.next() {
                            match c {
                                '"' => break,
                                '\\' => match chars.next() {
                                    Some('n') => literal.push('\n'),
                                    Some('u') => {
                                        let code: String = chars
                                            .by_ref()
                                            .skip(1)
                                            .take_while(|c| *c != '}')
                                            .collect();
                                        let code = u32::from_str_radix(&code, 16).unwrap();
                                        literal.push(char::from_u32(code).unwrap());
                                    }
                                    Some(other) => literal.push(other),
                                    None => {}
                                },
                                c => literal.push(c),
                            }
                        }
                        found.push(literal);
                    }
                }
            }
        }
        found.sort();
        found.dedup();
        found
    }

    #[test]
    fn every_string_of_the_interface_has_its_russian_and_none_is_left_over() {
        let used = used();
        assert!(used.len() > 50, "the sources were not read: {used:?}");
        let table: HashMap<&str, &str> = ru::WORDS.iter().copied().collect();
        assert_eq!(
            table.len(),
            ru::WORDS.len(),
            "a string is in the table twice"
        );
        let missing: Vec<&String> = used
            .iter()
            .filter(|text| !table.contains_key(text.as_str()))
            .collect();
        assert!(missing.is_empty(), "no Russian for: {missing:#?}");
        let stale: Vec<&&str> = table
            .keys()
            .filter(|text| !used.iter().any(|used| used == **text))
            .collect();
        assert!(stale.is_empty(), "Russian for what is not used: {stale:#?}");
        // A part put into a string is put into its Russian too.
        for (english, russian) in ru::WORDS {
            let parts = |text: &str| text.matches('{').count();
            assert_eq!(parts(english), parts(russian), "{english}");
            assert!(!russian.contains('\u{2014}'), "a long dash in: {russian}");
        }
    }

    #[gpui::test]
    fn every_command_of_the_palette_has_a_russian_name(cx: &mut gpui::TestAppContext) {
        use crate::command_palette::{humanize_action_name, is_listed};
        let names: Vec<String> = cx.update(|cx| {
            cx.all_action_names()
                .iter()
                // The framework's own "no action" is not a command.
                .filter(|name| is_listed(name) && !name.starts_with("zed::"))
                .map(|name| humanize_action_name(name))
                .collect()
        });
        assert!(names.len() > 200, "{names:?}");
        let table: HashMap<&str, &str> = ru::COMMANDS.iter().copied().collect();
        assert_eq!(
            table.len(),
            ru::COMMANDS.len(),
            "a command is in the table twice"
        );
        let missing: Vec<&String> = names
            .iter()
            .filter(|name| !table.contains_key(name.as_str()))
            .collect();
        assert!(missing.is_empty(), "no Russian for: {missing:#?}");
        let stale: Vec<&&str> = table
            .keys()
            .filter(|name| !names.iter().any(|known| known == **name))
            .collect();
        assert!(
            stale.is_empty(),
            "Russian for commands that are gone: {stale:#?}"
        );
    }
}
