//! Themes of other editors as Solder's own: a VS Code or Zed theme's
//! palette laid over Solder's tokens, with what a theme does not say worked
//! out from its background and text.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::jsonc;

/// A theme as Solder keeps it in `themes/<name>.json`. Colors are
/// `#rrggbb` or `#rrggbbaa`, keyed by the token's name in the editor
/// (`bg`, `fg_muted`, `accent`, ...; `keyword`, `string`, ... for syntax;
/// `red`, `bright_blue`, ... for the terminal).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeFile {
    pub name: String,
    /// `dark` or `light`: what tokens left out fall back to.
    pub appearance: String,
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub syntax: BTreeMap<String, String>,
    /// The sixteen colors programs in the terminal ask for by number.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub terminal: BTreeMap<String, String>,
}

pub const COLORS: &[&str] = &[
    "bg",
    "bg_elev",
    "bg_sunken",
    "fg",
    "fg_muted",
    "fg_subtle",
    "line",
    "accent",
    "accent_soft",
    "accent_fg",
    "selection",
    "active_line",
    "search_match",
    "search_active",
    "bracket",
    "error",
    "warning",
    "git_added",
    "git_modified",
    "conflict_ours",
    "conflict_theirs",
];

pub const SYNTAX: &[&str] = &[
    "keyword",
    "string",
    "function",
    "type",
    "comment",
    "number",
    "punctuation",
    "variable",
    "tag",
];

/// The terminal's colors, in the order programs number them.
pub const TERMINAL: &[&str] = &[
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright_black",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_white",
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`.
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.trim().strip_prefix('#')?;
        if !hex.is_ascii() {
            return None;
        }
        let digit = |i: usize, wide: bool| -> Option<u8> {
            if wide {
                u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()
            } else {
                let v = u8::from_str_radix(hex.get(i..i + 1)?, 16).ok()?;
                Some(v * 17)
            }
        };
        let (wide, alpha) = match hex.len() {
            3 => (false, false),
            4 => (false, true),
            6 => (true, false),
            8 => (true, true),
            _ => return None,
        };
        Some(Self {
            r: digit(0, wide)?,
            g: digit(1, wide)?,
            b: digit(2, wide)?,
            a: if alpha { digit(3, wide)? } else { 255 },
        })
    }

    pub fn hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }

    fn alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// `self` moved `t` of the way to `other`, opaque.
    fn mix(self, other: Self, t: f32) -> Self {
        let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Self {
            r: lerp(self.r, other.r),
            g: lerp(self.g, other.g),
            b: lerp(self.b, other.b),
            a: 255,
        }
    }

    /// Relative luminance, 0 (black) to 1 (white).
    fn luminance(self) -> f32 {
        let channel = |c: u8| {
            let c = c as f32 / 255.;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }

    fn contrast(self, other: Self) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Black or white, whichever reads better on this color.
    fn readable_on(self) -> Self {
        if self.contrast(BLACK) >= self.contrast(WHITE) {
            BLACK
        } else {
            WHITE
        }
    }

    /// How far from grey, 0 to 1.
    fn saturation(self) -> f32 {
        let max = self.r.max(self.g).max(self.b) as f32;
        let min = self.r.min(self.g).min(self.b) as f32;
        if max == 0. { 0. } else { (max - min) / max }
    }
}

const BLACK: Rgba = Rgba {
    r: 0,
    g: 0,
    b: 0,
    a: 255,
};
const WHITE: Rgba = Rgba {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};

/// The colors read so far, by Solder's token.
#[derive(Default)]
struct Palette {
    colors: BTreeMap<&'static str, Rgba>,
    syntax: BTreeMap<&'static str, Rgba>,
    terminal: BTreeMap<&'static str, Rgba>,
}

impl Palette {
    /// The theme, with every token the source left out worked out from the
    /// background and the text. `None` without those two.
    fn finish(mut self, name: &str, appearance: Option<&str>) -> Option<ThemeFile> {
        let bg = *self.colors.get("bg")?;
        let fg = *self.colors.get("fg")?;
        let dark = match appearance {
            Some("dark") => true,
            Some("light") => false,
            _ => bg.luminance() < 0.5,
        };
        // A border that stands out against the page (some themes use their
        // brightest color) would draw over everything here.
        if self.colors.get("line").is_some_and(|l| l.contrast(bg) > 2.) {
            self.colors.remove("line");
        }
        // Solder writes hints and labels in these two; a theme's color for
        // line numbers alone can be too faint to read.
        for (token, least) in [("fg_muted", 3.), ("fg_subtle", 2.5)] {
            if self
                .colors
                .get(token)
                .is_some_and(|c| c.contrast(bg) < least)
            {
                self.colors.remove(token);
            }
        }
        // A grey "accent" is a button color, not an accent.
        if self
            .colors
            .get("accent")
            .is_some_and(|a| a.saturation() < 0.25)
        {
            self.colors.remove("accent");
        }
        let syntax_fallback = |palette: &Palette, key: &str| palette.syntax.get(key).copied();
        let accent = self
            .colors
            .get("accent")
            .copied()
            .or_else(|| syntax_fallback(&self, "function"))
            .or_else(|| syntax_fallback(&self, "keyword"))
            .unwrap_or(fg);
        let derived: [(&'static str, Rgba); 19] = [
            ("bg_elev", bg.mix(fg, 0.06)),
            ("bg_sunken", bg.mix(BLACK, if dark { 0.22 } else { 0.04 })),
            ("fg_muted", fg.mix(bg, 0.3)),
            ("fg_subtle", fg.mix(bg, 0.55)),
            ("line", bg.mix(fg, 0.12)),
            ("accent", accent),
            ("accent_soft", accent.alpha(0x2e)),
            ("accent_fg", accent.readable_on()),
            ("selection", accent.alpha(0x40)),
            ("active_line", bg.mix(fg, 0.05)),
            ("search_match", accent.alpha(0x38)),
            ("search_active", accent.alpha(0x70)),
            ("bracket", fg.alpha(0x30)),
            (
                "error",
                Rgba::parse(if dark { "#f87171" } else { "#dc2626" })?,
            ),
            (
                "warning",
                Rgba::parse(if dark { "#fbbf24" } else { "#b45309" })?,
            ),
            (
                "git_added",
                Rgba::parse(if dark { "#4ade80" } else { "#15803d" })?,
            ),
            (
                "git_modified",
                Rgba::parse(if dark { "#60a5fa" } else { "#1d4ed8" })?,
            ),
            ("conflict_ours", Rgba::parse("#4ade80")?.alpha(0x30)),
            ("conflict_theirs", Rgba::parse("#60a5fa")?.alpha(0x30)),
        ];
        for (key, color) in derived {
            self.colors.entry(key).or_insert(color);
        }
        // The text on an accent button must be readable on it.
        let accent = self.colors["accent"];
        if self.colors["accent_fg"].contrast(accent) < 3. {
            self.colors.insert("accent_fg", accent.readable_on());
        }
        let muted = self.colors["fg_muted"];
        for (key, color) in [
            ("punctuation", muted),
            ("comment", self.colors["fg_subtle"]),
        ] {
            self.syntax.entry(key).or_insert(color);
        }
        for key in SYNTAX {
            self.syntax.entry(key).or_insert(fg);
        }
        let hex = |map: BTreeMap<&'static str, Rgba>| {
            map.into_iter()
                .map(|(key, color)| (key.to_string(), color.hex()))
                .collect()
        };
        Some(ThemeFile {
            name: name.to_string(),
            appearance: if dark { "dark" } else { "light" }.into(),
            colors: hex(self.colors),
            syntax: hex(self.syntax),
            // Only the ones the source has: the rest are the editor's.
            terminal: hex(self.terminal),
        })
    }
}

// ---------------------------------------------------------------- VS Code

/// Solder's tokens and the VS Code colors that fill them, best first.
const VSCODE_COLORS: &[(&str, &[&str])] = &[
    ("bg", &["editor.background"]),
    (
        "bg_elev",
        &[
            "editorWidget.background",
            "dropdown.background",
            "input.background",
        ],
    ),
    (
        "bg_sunken",
        &[
            "sideBar.background",
            "activityBar.background",
            "panel.background",
        ],
    ),
    ("fg", &["editor.foreground", "foreground"]),
    ("fg_muted", &["sideBar.foreground", "descriptionForeground"]),
    (
        "fg_subtle",
        &["editorLineNumber.foreground", "tab.inactiveForeground"],
    ),
    (
        "line",
        &[
            "sideBar.border",
            "tab.border",
            "editorGroup.border",
            "panel.border",
        ],
    ),
    (
        "accent",
        &[
            "activityBarBadge.background",
            "button.background",
            "focusBorder",
            "textLink.foreground",
            "progressBar.background",
        ],
    ),
    (
        "accent_fg",
        &["activityBarBadge.foreground", "button.foreground"],
    ),
    ("selection", &["editor.selectionBackground"]),
    ("active_line", &["editor.lineHighlightBackground"]),
    ("search_match", &["editor.findMatchHighlightBackground"]),
    ("search_active", &["editor.findMatchBackground"]),
    (
        "bracket",
        &["editorBracketMatch.background", "editorBracketMatch.border"],
    ),
    ("error", &["editorError.foreground", "errorForeground"]),
    (
        "warning",
        &["editorWarning.foreground", "list.warningForeground"],
    ),
    (
        "git_added",
        &[
            "gitDecoration.addedResourceForeground",
            "gitDecoration.untrackedResourceForeground",
            "editorGutter.addedBackground",
        ],
    ),
    (
        "git_modified",
        &[
            "gitDecoration.modifiedResourceForeground",
            "editorGutter.modifiedBackground",
        ],
    ),
    ("conflict_ours", &["merge.currentHeaderBackground"]),
    ("conflict_theirs", &["merge.incomingHeaderBackground"]),
];

/// Solder's syntax tokens and the TextMate scopes that color them, best
/// first.
const VSCODE_SCOPES: &[(&str, &[&str])] = &[
    (
        "keyword",
        &["keyword", "keyword.control", "storage.type", "storage"],
    ),
    ("string", &["string", "string.quoted"]),
    (
        "function",
        &[
            "entity.name.function",
            "support.function",
            "meta.function-call",
        ],
    ),
    (
        "type",
        &[
            "entity.name.type",
            "entity.name.class",
            "support.type",
            "support.class",
        ],
    ),
    ("comment", &["comment"]),
    ("number", &["constant.numeric", "constant"]),
    ("punctuation", &["punctuation", "meta.brace"]),
    ("variable", &["variable", "variable.other"]),
    ("tag", &["entity.name.tag"]),
];

/// One selector of a theme's rule: its innermost scope, its color, and
/// whether it only holds inside another scope (`source.shell string`).
struct Rule {
    scope: String,
    color: Rgba,
    nested: bool,
}

/// The color a theme's rules give `scope`: the rule whose selector is the
/// longest prefix of it, the later one among equals. A selector that holds
/// everywhere beats one for a place (strings in a shell loop).
fn scope_color(rules: &[Rule], scope: &str) -> Option<Rgba> {
    let covers = |rule: &&Rule| {
        scope == rule.scope
            || scope
                .strip_prefix(rule.scope.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
    };
    let best = |nested: bool| {
        rules
            .iter()
            .filter(|rule| rule.nested == nested)
            .filter(covers)
            .max_by_key(|rule| rule.scope.len())
            .map(|rule| rule.color)
    };
    best(false).or_else(|| best(true)).or_else(|| {
        // A theme may only color a narrower scope (`keyword.control`).
        rules
            .iter()
            .rev()
            .filter(|rule| !rule.nested)
            .find(|rule| {
                rule.scope
                    .strip_prefix(scope)
                    .is_some_and(|rest| rest.starts_with('.'))
            })
            .map(|rule| rule.color)
    })
}

/// A VS Code color theme (the JSON, with what it includes already merged).
/// `ui_theme` is the manifest's `vs-dark`, `vs` or `hc-black`.
pub fn from_vscode(name: &str, theme: &Value, ui_theme: Option<&str>) -> Option<ThemeFile> {
    let mut palette = Palette::default();
    let colors = &theme["colors"];
    for (token, keys) in VSCODE_COLORS {
        let found = keys
            .iter()
            .find_map(|key| Rgba::parse(colors[*key].as_str()?));
        if let Some(color) = found {
            palette.colors.insert(token, color);
        }
    }
    // Selectors in order, each with its innermost scope: `source.js string`
    // colors strings.
    let mut rules: Vec<Rule> = Vec::new();
    for rule in theme["tokenColors"].as_array().into_iter().flatten() {
        let Some(color) = rule["settings"]["foreground"]
            .as_str()
            .and_then(Rgba::parse)
        else {
            continue;
        };
        let scopes: Vec<&str> = match &rule["scope"] {
            Value::String(s) => s.split(',').collect(),
            Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
            _ => continue,
        };
        for scope in scopes {
            let mut parts = scope.split_whitespace();
            if let Some(last) = parts.next_back() {
                rules.push(Rule {
                    scope: last.to_string(),
                    color,
                    nested: parts.next().is_some(),
                });
            }
        }
    }
    for (token, scopes) in VSCODE_SCOPES {
        if let Some(color) = scopes.iter().find_map(|s| scope_color(&rules, s)) {
            palette.syntax.insert(token, color);
        }
    }
    // `bright_red` is `terminal.ansiBrightRed` there.
    for token in TERMINAL {
        let key: String = token
            .split('_')
            .map(|word| {
                let mut letters = word.chars();
                letters.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(letters).collect()
                })
            })
            .collect();
        if let Some(color) = colors[format!("terminal.ansi{key}").as_str()]
            .as_str()
            .and_then(Rgba::parse)
        {
            palette.terminal.insert(token, color);
        }
    }
    let appearance = match ui_theme.or(theme["type"].as_str()) {
        Some("vs-dark" | "hc-black" | "dark") => Some("dark"),
        Some("vs" | "hc-light" | "light") => Some("light"),
        _ => None,
    };
    palette.finish(name, appearance)
}

/// Reads a VS Code theme file, following `include`; blocking.
fn read_vscode(path: &Path, depth: usize) -> Option<Value> {
    let mut theme = jsonc::parse(&std::fs::read_to_string(path).ok()?).ok()?;
    let included = theme["include"].as_str().map(str::to_string);
    if let Some(include) = included.filter(|_| depth < 4)
        && let Some(base) = read_vscode(&path.parent()?.join(include), depth + 1)
    {
        // The including file wins, color by color; its rules come later.
        let mut merged = base;
        if let Some(colors) = theme["colors"].as_object() {
            for (key, value) in colors {
                merged["colors"][key] = value.clone();
            }
        }
        if let Some(rules) = theme["tokenColors"].as_array() {
            let mut all = merged["tokenColors"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            all.extend(rules.iter().cloned());
            merged["tokenColors"] = all.into();
        }
        if theme.get("type").is_some() {
            merged["type"] = theme["type"].clone();
        }
        theme = merged;
    }
    Some(theme)
}

/// The theme VS Code or Cursor calls `label`, looked for in the extensions
/// under each of `extension_dirs`; blocking.
pub fn find_vscode(label: &str, extension_dirs: &[PathBuf]) -> Option<ThemeFile> {
    for dir in extension_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut extensions: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        // The newest version of an extension last, so it is the one found.
        extensions.sort();
        for extension in extensions.iter().rev() {
            let Some(manifest) = std::fs::read_to_string(extension.join("package.json"))
                .ok()
                .and_then(|text| jsonc::parse(&text).ok())
            else {
                continue;
            };
            for theme in manifest["contributes"]["themes"]
                .as_array()
                .into_iter()
                .flatten()
            {
                // Built-in themes are named by `id`; their label is a key
                // into a translation file.
                if theme["label"].as_str() != Some(label) && theme["id"].as_str() != Some(label) {
                    continue;
                }
                let path = extension.join(theme["path"].as_str()?);
                let found = read_vscode(&path, 0)
                    .and_then(|json| from_vscode(label, &json, theme["uiTheme"].as_str()));
                if found.is_some() {
                    return found;
                }
            }
        }
    }
    None
}

// -------------------------------------------------------------------- Zed

const ZED_COLORS: &[(&str, &[&str])] = &[
    ("bg", &["editor.background", "background"]),
    (
        "bg_elev",
        &["elevated_surface.background", "element.background"],
    ),
    (
        "bg_sunken",
        &[
            "panel.background",
            "surface.background",
            "status_bar.background",
        ],
    ),
    ("fg", &["editor.foreground", "text"]),
    ("fg_muted", &["text.muted"]),
    ("fg_subtle", &["editor.line_number", "text.placeholder"]),
    ("line", &["border", "border.variant"]),
    ("accent", &["text.accent"]),
    ("active_line", &["editor.active_line.background"]),
    ("search_match", &["search.match_background"]),
    ("bracket", &["editor.document_highlight.read_background"]),
    ("error", &["error"]),
    ("warning", &["warning"]),
    ("git_added", &["created"]),
    ("git_modified", &["modified"]),
];

/// One theme of a Zed theme file: an entry of its `themes`.
pub fn from_zed(theme: &Value) -> Option<ThemeFile> {
    let style = &theme["style"];
    let mut palette = Palette::default();
    for (token, keys) in ZED_COLORS {
        let found = keys
            .iter()
            .find_map(|key| Rgba::parse(style[*key].as_str()?));
        if let Some(color) = found {
            palette.colors.insert(token, color);
        }
    }
    if let Some(selection) = style["players"][0]["selection"]
        .as_str()
        .and_then(Rgba::parse)
    {
        palette.colors.insert("selection", selection);
    }
    for token in SYNTAX {
        if let Some(color) = style["syntax"][*token]["color"]
            .as_str()
            .and_then(Rgba::parse)
        {
            palette.syntax.insert(token, color);
        }
    }
    for token in TERMINAL {
        if let Some(color) = style[format!("terminal.ansi.{token}").as_str()]
            .as_str()
            .and_then(Rgba::parse)
        {
            palette.terminal.insert(token, color);
        }
    }
    palette.finish(theme["name"].as_str()?, theme["appearance"].as_str())
}

/// Every theme of the Zed theme file at `path`; blocking.
pub fn all_zed(path: &Path) -> Vec<ThemeFile> {
    let Some(file) = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| jsonc::parse(&text).ok())
    else {
        return Vec::new();
    };
    file["themes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(from_zed)
        .collect()
}

/// Every theme the VS Code extension in `dir` contributes, named by its
/// label; blocking. `label` turns a `%key%` label into its text.
pub fn all_vscode(dir: &Path, manifest: &Value, label: impl Fn(&str) -> String) -> Vec<ThemeFile> {
    manifest["contributes"]["themes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|theme| {
            let name = label(theme["label"].as_str().or(theme["id"].as_str())?);
            let path = dir.join(theme["path"].as_str()?);
            from_vscode(&name, &read_vscode(&path, 0)?, theme["uiTheme"].as_str())
        })
        .collect()
}

/// The theme Zed calls `name`, looked for in every `.json` directly in
/// `theme_dirs`; blocking.
pub fn find_zed(name: &str, theme_dirs: &[PathBuf]) -> Option<ThemeFile> {
    for dir in theme_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(file) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| jsonc::parse(&text).ok())
            else {
                continue;
            };
            let found = file["themes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|theme| theme["name"].as_str() == Some(name))
                .and_then(from_zed);
            if found.is_some() {
                return found;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_hex_colors() {
        let c = Rgba::parse("#1a2B3c").unwrap();
        assert_eq!((c.r, c.g, c.b, c.a), (0x1a, 0x2b, 0x3c, 255));
        assert_eq!(c.hex(), "#1a2b3c");
        assert_eq!(Rgba::parse("#fff").unwrap().hex(), "#ffffff");
        assert_eq!(Rgba::parse("#0008").unwrap().hex(), "#00000088");
        assert_eq!(Rgba::parse("#FFB86C80").unwrap().hex(), "#ffb86c80");
        for bad in ["", "fff", "#ff", "#gggggg", "#12345", "red", "#ééé"] {
            assert_eq!(Rgba::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_vscode_theme_becomes_a_complete_theme() {
        let theme = json!({
            "colors": {
                "editor.background": "#282A36",
                "editor.foreground": "#F8F8F2",
                "sideBar.background": "#21222C",
                "editorLineNumber.foreground": "#6272A4",
                // Too loud for a border here; `tab.border` is used.
                "editorGroup.border": "#BD93F9",
                "tab.border": "#191A21",
                // A grey button is not an accent; the badge is.
                "activityBarBadge.background": "#FF79C6",
                "button.background": "#44475A",
                "editor.selectionBackground": "#44475A",
                "editor.findMatchBackground": "#FFB86C80",
                "editorError.foreground": "#FF5555",
                "terminal.ansiRed": "#FF5555",
                "terminal.ansiBrightBlue": "#D6ACFF",
            },
            "tokenColors": [
                {"scope": ["keyword", "storage"], "settings": {"foreground": "#FF79C6"}},
                {"scope": "string, string.quoted.double", "settings": {"foreground": "#F1FA8C"}},
                {"scope": "source.js entity.name.function", "settings": {"foreground": "#50FA7B"}},
                // Strings in one place only: not the color of strings.
                {"scope": ["meta.scope.for-loop.shell string"], "settings": {"foreground": "#F8F8F2"}},
                {"scope": ["comment"], "settings": {"foreground": "#6272A4", "fontStyle": "italic"}},
                {"scope": ["constant"], "settings": {"foreground": "#BD93F9"}},
                {"scope": ["constant.numeric.hex"], "settings": {"foreground": "#111111"}},
                {"scope": ["emphasis"], "settings": {"fontStyle": "italic"}},
            ],
        });
        let file = from_vscode("Dracula Theme", &theme, Some("vs-dark")).unwrap();
        assert_eq!(
            (file.name.as_str(), file.appearance.as_str()),
            ("Dracula Theme", "dark")
        );
        for (token, color) in [
            ("bg", "#282a36"),
            ("fg", "#f8f8f2"),
            ("bg_sunken", "#21222c"),
            ("fg_subtle", "#6272a4"),
            ("line", "#191a21"),
            ("accent", "#ff79c6"),
            ("accent_soft", "#ff79c62e"),
            ("accent_fg", "#000000"),
            ("selection", "#44475a"),
            ("search_active", "#ffb86c80"),
            ("error", "#ff5555"),
        ] {
            assert_eq!(file.colors[token], color, "{token}");
        }
        for (token, color) in [
            ("keyword", "#ff79c6"),
            ("string", "#f1fa8c"),
            ("function", "#50fa7b"),
            ("comment", "#6272a4"),
            // `constant` colors numbers; the narrower hex rule does not.
            ("number", "#bd93f9"),
            // Not in the theme: the text color.
            ("variable", "#f8f8f2"),
        ] {
            assert_eq!(file.syntax[token], color, "{token}");
        }
        // Every token is there, so the file stands on its own.
        assert!(COLORS.iter().all(|t| file.colors.contains_key(*t)));
        assert!(SYNTAX.iter().all(|t| file.syntax.contains_key(*t)));
        // The terminal's colors the theme has, and no others.
        assert_eq!(file.terminal["red"], "#ff5555");
        assert_eq!(file.terminal["bright_blue"], "#d6acff");
        assert_eq!(file.terminal.len(), 2);
        // Without a background there is no theme.
        assert_eq!(from_vscode("x", &json!({"colors": {}}), None), None);
        // Light or dark is read off the background when nothing says.
        let light =
            json!({"colors": {"editor.background": "#ffffff", "editor.foreground": "#222222"}});
        assert_eq!(from_vscode("x", &light, None).unwrap().appearance, "light");
    }

    #[test]
    fn a_zed_theme_becomes_a_complete_theme() {
        let theme = json!({
            "name": "Tokyo Night",
            "appearance": "dark",
            "style": {
                "background": "#16161e",
                "editor.background": "#1a1b26",
                "text": "#a9b1d6",
                "text.muted": "#787c99",
                "text.accent": "#7dcfff",
                "border": "#101014",
                "editor.active_line.background": "#1e202e",
                "created": "#9ece6a",
                "terminal.ansi.green": "#73daca",
                "terminal.ansi.bright_black": "#414868",
                "players": [{"cursor": "#7c7f93", "selection": "#267ead3d"}],
                "syntax": {
                    "keyword": {"color": "#bb9af7", "font_style": null},
                    "string": {"color": "#9ece6a"},
                },
            },
        });
        let file = from_zed(&theme).unwrap();
        assert_eq!(
            (file.name.as_str(), file.appearance.as_str()),
            ("Tokyo Night", "dark")
        );
        for (token, color) in [
            ("bg", "#1a1b26"),
            ("fg", "#a9b1d6"),
            ("fg_muted", "#787c99"),
            ("accent", "#7dcfff"),
            ("line", "#101014"),
            ("active_line", "#1e202e"),
            ("selection", "#267ead3d"),
            ("git_added", "#9ece6a"),
        ] {
            assert_eq!(file.colors[token], color, "{token}");
        }
        assert_eq!(file.syntax["keyword"], "#bb9af7");
        assert_eq!(file.syntax["string"], "#9ece6a");
        assert!(COLORS.iter().all(|t| file.colors.contains_key(*t)));
        assert_eq!(file.terminal["green"], "#73daca");
        assert_eq!(file.terminal["bright_black"], "#414868");
        assert_eq!(file.terminal.len(), 2);

        // Line numbers too faint to read as text are not used for text.
        let mut faint = theme.clone();
        faint["style"]["editor.line_number"] = json!("#363b54");
        let file = from_zed(&faint).unwrap();
        assert_ne!(file.colors["fg_subtle"], "#363b54");
        let readable = Rgba::parse(&file.colors["fg_subtle"]).unwrap();
        assert!(readable.contrast(Rgba::parse("#1a1b26").unwrap()) >= 2.5);
    }

    #[test]
    fn finds_a_theme_by_its_label_and_follows_includes() {
        let dir = std::env::temp_dir().join(format!("solder-import-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ext = dir.join("extensions/pub.theme-1.0.0");
        std::fs::create_dir_all(ext.join("themes")).unwrap();
        std::fs::write(
            ext.join("package.json"),
            r#"{"contributes": {"themes": [
                {"label": "Other", "uiTheme": "vs", "path": "./themes/missing.json"},
                {"label": "Night Owl", "uiTheme": "vs-dark", "path": "./themes/owl.json"}]}}"#,
        )
        .unwrap();
        std::fs::write(
            ext.join("themes/base.json"),
            r##"{"colors": {"editor.background": "#011627", "editor.foreground": "#000000"},
                "tokenColors": [{"scope": "string", "settings": {"foreground": "#111111"}}]}"##,
        )
        .unwrap();
        std::fs::write(
            ext.join("themes/owl.json"),
            r##"// Night Owl
            {"include": "./base.json",
             "colors": {"editor.foreground": "#d6deeb",},
             "tokenColors": [{"scope": "string", "settings": {"foreground": "#ecc48d"}}]}"##,
        )
        .unwrap();
        let dirs = [dir.join("nowhere"), dir.join("extensions")];
        let theme = find_vscode("Night Owl", &dirs).unwrap();
        assert_eq!(theme.colors["bg"], "#011627");
        assert_eq!(theme.colors["fg"], "#d6deeb");
        assert_eq!(theme.syntax["string"], "#ecc48d");
        assert_eq!(find_vscode("Nope", &dirs), None);
        assert_eq!(find_vscode("Other", &dirs), None);

        let zed = dir.join("zed");
        std::fs::create_dir_all(&zed).unwrap();
        std::fs::write(
            zed.join("ayu.json"),
            r##"{"name": "Ayu", "themes": [
                {"name": "Ayu Light", "appearance": "light", "style": {"background": "#fcfcfc", "text": "#5c6166"}},
                {"name": "Ayu Dark", "appearance": "dark", "style": {"background": "#0d1017", "text": "#bfbdb6"}}]}"##,
        )
        .unwrap();
        assert_eq!(
            find_zed("Ayu Dark", std::slice::from_ref(&zed))
                .unwrap()
                .colors["bg"],
            "#0d1017"
        );
        assert_eq!(find_zed("Ayu Mirage", &[zed]), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
