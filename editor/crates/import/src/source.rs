//! Finds other editors' config on this machine and reads from each what
//! Solder can take over: settings, the theme, key bindings, and which of
//! its extensions Solder already covers. Everything here blocks on files.

use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{Value, json};

use crate::{
    jsonc,
    keymap::{self, Binding, Converted},
    theme::{self, ThemeFile},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    VsCode,
    Cursor,
    Zed,
    JetBrains,
}

/// An editor found on this machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub kind: Kind,
    /// `VS Code`, `Cursor`, `Zed`, `WebStorm 2025.1`.
    pub name: String,
    /// Where it keeps its settings.
    pub dir: PathBuf,
}

/// Where to look. Tests point these at a folder of their own.
#[derive(Clone, Debug)]
pub struct Roots {
    pub home: PathBuf,
    /// Extension folders inside the editors' own installations, where the
    /// themes they ship with live.
    pub bundled: Vec<PathBuf>,
}

impl Roots {
    pub fn system(home: PathBuf) -> Self {
        let bundled = [
            "/Applications/Visual Studio Code.app/Contents/Resources/app/extensions",
            "/Applications/Cursor.app/Contents/Resources/app/extensions",
            "/usr/share/code/resources/app/extensions",
            "/opt/visual-studio-code/resources/app/extensions",
            "/opt/Cursor/resources/app/extensions",
        ]
        .map(PathBuf::from)
        .to_vec();
        Self { home, bundled }
    }

    /// Per-user config folders: macOS's, then the XDG one.
    fn config(&self, name: &str) -> [PathBuf; 2] {
        [
            self.home.join("Library/Application Support").join(name),
            self.home.join(".config").join(name),
        ]
    }
}

/// The editors with something to import, in the order they are offered.
pub fn detect(roots: &Roots) -> Vec<Source> {
    let mut found = Vec::new();
    for (kind, name, folder) in [
        (Kind::VsCode, "VS Code", "Code"),
        (Kind::Cursor, "Cursor", "Cursor"),
    ] {
        let dir = roots
            .config(folder)
            .into_iter()
            .map(|d| d.join("User"))
            .find(|d| d.join("settings.json").is_file() || d.join("keybindings.json").is_file());
        if let Some(dir) = dir {
            found.push(Source {
                kind,
                name: name.into(),
                dir,
            });
        }
    }
    let zed = roots.home.join(".config/zed");
    if zed.join("settings.json").is_file() || zed.join("keymap.json").is_file() {
        found.push(Source {
            kind: Kind::Zed,
            name: "Zed".into(),
            dir: zed,
        });
    }
    // One folder per product and version: `WebStorm2025.1`. The newest of
    // each product is the one in use.
    let mut products: Vec<(String, String, PathBuf)> = roots
        .config("JetBrains")
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let folder = entry.file_name().to_string_lossy().into_owned();
            let split = folder.find(|c: char| c.is_ascii_digit())?;
            let (product, version) = folder.split_at(split);
            (dir.join("options").is_dir() || dir.join("keymaps").is_dir())
                .then(|| (product.to_string(), version.to_string(), dir))
        })
        .collect();
    products.sort();
    products.reverse();
    products.dedup_by(|later, first| later.0 == first.0);
    products.reverse();
    for (product, version, dir) in products {
        let product = match product.as_str() {
            "IntelliJIdea" | "IdeaIC" => "IntelliJ IDEA",
            other => other,
        };
        found.push(Source {
            kind: Kind::JetBrains,
            name: format!("{product} {version}"),
            dir,
        });
    }
    found
}

/// One setting to bring over.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    /// The key in Solder's `settings.json`.
    pub key: &'static str,
    /// What it is, for the list: `Font size`.
    pub label: &'static str,
    pub value: Value,
}

impl Setting {
    /// The value as the list shows it.
    pub fn shown(&self) -> String {
        match &self.value {
            Value::String(text) => text.clone(),
            Value::Bool(true) => "on".into(),
            Value::Bool(false) => "off".into(),
            other => other.to_string(),
        }
    }
}

/// An extension the user has there, and what does its job here.
#[derive(Clone, Debug, PartialEq)]
pub struct Hint {
    pub extension: String,
    pub instead: &'static str,
}

/// Everything one editor has to offer.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub source: Source,
    pub settings: Vec<Setting>,
    /// The theme in use there, and the theme itself when its file was found.
    pub theme_name: Option<String>,
    pub theme: Option<ThemeFile>,
    /// The editor's own keys for the actions Solder has, and what to call
    /// them.
    pub preset_name: Option<&'static str>,
    pub preset: Vec<Binding>,
    /// The bindings the user changed there.
    pub custom: Converted,
    pub hints: Vec<Hint>,
}

fn setting(key: &'static str, label: &'static str, value: impl Into<Value>) -> Setting {
    Setting {
        key,
        label,
        value: value.into(),
    }
}

/// The first family of a CSS font list: `'Fira Code', Menlo, monospace`.
fn first_font(list: &str) -> Option<String> {
    let first = list
        .split(',')
        .next()?
        .trim()
        .trim_matches(['\'', '"'])
        .trim();
    (!first.is_empty() && first != "monospace").then(|| first.to_string())
}

/// Solder indents by 2, 4 or 8.
fn indent(value: &Value) -> Option<Setting> {
    let size = value.as_u64()?;
    matches!(size, 2 | 4 | 8).then(|| setting("indent_size", "Indent", size))
}

fn read_jsonc(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| jsonc::parse(&text).ok())
        .unwrap_or(Value::Null)
}

/// What `source` has. `mac` picks the editor's keys for this platform.
pub fn plan(source: &Source, roots: &Roots, mac: bool) -> Plan {
    let mut plan = Plan {
        source: source.clone(),
        settings: Vec::new(),
        theme_name: None,
        theme: None,
        preset_name: None,
        preset: Vec::new(),
        custom: Converted::default(),
        hints: Vec::new(),
    };
    match source.kind {
        Kind::VsCode | Kind::Cursor => vscode(&mut plan, roots, mac),
        Kind::Zed => zed(&mut plan, roots),
        Kind::JetBrains => jetbrains(&mut plan, mac),
    }
    plan
}

fn vscode(plan: &mut Plan, roots: &Roots, mac: bool) {
    let settings = read_jsonc(&plan.source.dir.join("settings.json"));
    let size = settings["editor.fontSize"]
        .as_f64()
        .filter(|s| (6. ..=72.).contains(s));
    if let Some(family) = settings["editor.fontFamily"].as_str().and_then(first_font) {
        plan.settings
            .push(setting("buffer_font_family", "Font", family));
    }
    if let Some(size) = size {
        plan.settings
            .push(setting("buffer_font_size", "Font size", size));
    }
    // Below 8 it is a multiple of the font size, above it pixels; 0 means
    // the editor's own choice.
    if let Some(height) = settings["editor.lineHeight"].as_f64().filter(|h| *h > 0.) {
        let pixels = if height < 8. {
            height * size.unwrap_or(12.)
        } else {
            height
        };
        plan.settings
            .push(setting("buffer_line_height", "Line height", pixels.round()));
    }
    plan.settings.extend(indent(&settings["editor.tabSize"]));
    if let Some(format) = settings["editor.formatOnSave"].as_bool() {
        plan.settings
            .push(setting("format_on_save", "Format on save", format));
    }

    let extensions_dir = roots.home.join(match plan.source.kind {
        Kind::Cursor => ".cursor/extensions",
        _ => ".vscode/extensions",
    });
    plan.theme_name = settings["workbench.colorTheme"]
        .as_str()
        .map(str::to_string);
    if let Some(name) = &plan.theme_name {
        let mut dirs = vec![extensions_dir.clone()];
        dirs.extend(roots.bundled.iter().cloned());
        plan.theme = theme::find_vscode(name, &dirs);
    }

    plan.preset_name = Some("VS Code keys");
    plan.preset = keymap::vscode_preset(mac);
    plan.custom = keymap::from_vscode(&read_jsonc(&plan.source.dir.join("keybindings.json")));

    // The folders are `publisher.name-1.2.3`.
    let mut installed: Vec<String> = std::fs::read_dir(&extensions_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let folder = entry.file_name().to_string_lossy().to_ascii_lowercase();
            match folder.rfind('-') {
                Some(dash) if folder[dash + 1..].starts_with(|c: char| c.is_ascii_digit()) => {
                    folder[..dash].to_string()
                }
                _ => folder,
            }
        })
        .collect();
    installed.sort();
    installed.dedup();
    for id in installed {
        if let Some((_, instead)) = HINTS.iter().find(|(known, _)| *known == id) {
            plan.hints.push(Hint {
                extension: id,
                instead,
            });
        }
    }
}

/// Extensions people install in VS Code for what Solder has built in.
const HINTS: &[(&str, &str)] = &[
    (
        "eamodio.gitlens",
        "The Git tab: changes, staging by line, commits, conflicts",
    ),
    (
        "mhutchie.git-graph",
        "The Git tab: changes, staging by line, commits, conflicts",
    ),
    (
        "donjayamanne.githistory",
        "The Git tab: changes, staging by line, commits, conflicts",
    ),
    (
        "humao.rest-client",
        "The API tab sends requests from .http files",
    ),
    (
        "rangav.vscode-thunder-client",
        "The API tab sends requests from .http files",
    ),
    (
        "postman.postman-for-vscode",
        "The API tab sends requests from .http files",
    ),
    (
        "ms-azuretools.vscode-docker",
        "The Services tab runs the stack and lists containers",
    ),
    (
        "ms-azuretools.vscode-containers",
        "The Services tab runs the stack and lists containers",
    ),
    (
        "mtxr.sqltools",
        "The Database tab: Postgres, MySQL, SQLite, Redis, MongoDB",
    ),
    (
        "cweijan.vscode-database-client2",
        "The Database tab: Postgres, MySQL, SQLite, Redis, MongoDB",
    ),
    (
        "cweijan.vscode-mysql-client2",
        "The Database tab: Postgres, MySQL, SQLite, Redis, MongoDB",
    ),
    (
        "ckolkman.vscode-postgres",
        "The Database tab: Postgres, MySQL, SQLite, Redis, MongoDB",
    ),
    (
        "mongodb.mongodb-vscode",
        "The Database tab: Postgres, MySQL, SQLite, Redis, MongoDB",
    ),
    (
        "cweijan.vscode-redis-client",
        "The Database tab: Postgres, MySQL, SQLite, Redis, MongoDB",
    ),
    (
        "github.copilot",
        "Solder AI: completions, chat and agents, local or with your key",
    ),
    (
        "github.copilot-chat",
        "Solder AI: completions, chat and agents, local or with your key",
    ),
    (
        "continue.continue",
        "Solder AI: completions, chat and agents, local or with your key",
    ),
    (
        "codeium.codeium",
        "Solder AI: completions, chat and agents, local or with your key",
    ),
    (
        "supermaven.supermaven",
        "Solder AI: completions, chat and agents, local or with your key",
    ),
    (
        "tabnine.tabnine-vscode",
        "Solder AI: completions, chat and agents, local or with your key",
    ),
    (
        "rust-lang.rust-analyzer",
        "rust-analyzer runs when it is on your PATH",
    ),
    ("golang.go", "gopls runs when it is on your PATH"),
    ("ms-python.python", "pyright runs when it is on your PATH"),
    (
        "ms-python.vscode-pylance",
        "pyright runs when it is on your PATH",
    ),
    (
        "ms-vscode.js-debug-nightly",
        "The debugger runs JavaScript and TypeScript with js-debug",
    ),
    (
        "firefox-devtools.vscode-firefox-debug",
        "The debugger runs a server and its page in one session",
    ),
    (
        "msjsdiag.debugger-for-chrome",
        "The debugger runs a server and its page in one session",
    ),
];

fn zed(plan: &mut Plan, roots: &Roots) {
    let settings = read_jsonc(&plan.source.dir.join("settings.json"));
    let size = settings["buffer_font_size"]
        .as_f64()
        .filter(|s| (6. ..=72.).contains(s));
    // Zed's own fonts ship inside Zed.
    let family = settings["buffer_font_family"]
        .as_str()
        .filter(|f| !f.starts_with('.') && !f.starts_with("Zed "));
    if let Some(family) = family {
        plan.settings
            .push(setting("buffer_font_family", "Font", family));
    }
    if let Some(size) = size {
        plan.settings
            .push(setting("buffer_font_size", "Font size", size));
    }
    let ratio = match &settings["buffer_line_height"] {
        Value::String(name) if name == "comfortable" => Some(1.618),
        Value::String(name) if name == "standard" => Some(1.3),
        other => other["custom"].as_f64(),
    };
    if let Some(ratio) = ratio {
        let pixels = (ratio * size.unwrap_or(15.)).round();
        plan.settings
            .push(setting("buffer_line_height", "Line height", pixels));
    }
    plan.settings.extend(indent(&settings["tab_size"]));
    match settings["format_on_save"].as_str() {
        Some("on") => plan
            .settings
            .push(setting("format_on_save", "Format on save", true)),
        Some("off") => plan
            .settings
            .push(setting("format_on_save", "Format on save", false)),
        _ => {}
    }

    // One name, or one for each of light and dark with the mode to use.
    plan.theme_name = match &settings["theme"] {
        Value::String(name) => Some(name.clone()),
        theme => {
            let mode = if theme["mode"] == json!("light") {
                "light"
            } else {
                "dark"
            };
            theme[mode].as_str().map(str::to_string)
        }
    };
    if let Some(name) = &plan.theme_name {
        // The user's own themes, then those of installed extensions.
        let mut dirs = vec![plan.source.dir.join("themes")];
        for data in roots
            .config("Zed")
            .into_iter()
            .chain([roots.home.join(".local/share/zed")])
        {
            let installed = data.join("extensions/installed");
            for extension in std::fs::read_dir(installed).into_iter().flatten().flatten() {
                dirs.push(extension.path().join("themes"));
            }
        }
        plan.theme = theme::find_zed(name, &dirs);
    }
    plan.custom = keymap::from_zed(&read_jsonc(&plan.source.dir.join("keymap.json")));
}

fn jetbrains(plan: &mut Plan, mac: bool) {
    let option = |xml: &str, name: &str| {
        Regex::new(&format!(r#"<option\s+name="{name}"\s+value="([^"]*)""#))
            .ok()?
            .captures(xml)
            .map(|found| found[1].to_string())
    };
    if let Ok(font) = std::fs::read_to_string(plan.source.dir.join("options/editor-font.xml")) {
        if let Some(family) = option(&font, "FONT_FAMILY").filter(|f| !f.is_empty()) {
            plan.settings
                .push(setting("buffer_font_family", "Font", family));
        }
        let size = option(&font, "FONT_SIZE").and_then(|s| s.parse::<f64>().ok());
        if let Some(size) = size.filter(|s| (6. ..=72.).contains(s)) {
            plan.settings
                .push(setting("buffer_font_size", "Font size", size));
        }
    }
    plan.preset_name = Some("JetBrains keys");
    plan.preset = keymap::jetbrains_preset(mac);
    // The keymaps the user made; each holds what differs from its parent.
    let mut keymaps: Vec<PathBuf> = std::fs::read_dir(plan.source.dir.join("keymaps"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "xml"))
        .collect();
    keymaps.sort();
    for path in keymaps {
        if let Ok(xml) = std::fs::read_to_string(path) {
            let converted = keymap::from_jetbrains(&xml);
            plan.custom.bindings.extend(converted.bindings);
            plan.custom.skipped.extend(converted.skipped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home folder with what each editor keeps.
    fn home(name: &str) -> Roots {
        let home =
            std::env::temp_dir().join(format!("solder-import-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let write = |path: &str, text: &str| {
            let path = home.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "Library/Application Support/Code/User/settings.json",
            r#"{
              // The font, then a fallback.
              "editor.fontFamily": "'Fira Code', Menlo, monospace",
              "editor.fontSize": 14,
              "editor.lineHeight": 1.5,
              "editor.tabSize": 2,
              "editor.formatOnSave": true,
              "workbench.colorTheme": "Night Owl",
            }"#,
        );
        write(
            "Library/Application Support/Code/User/keybindings.json",
            r#"[{"key": "cmd+shift+d", "command": "editor.action.copyLinesDownAction"},
                {"key": "cmd+k z", "command": "workbench.action.toggleZenMode"}]"#,
        );
        write(
            ".vscode/extensions/sdras.night-owl-2.0.1/package.json",
            r#"{"contributes": {"themes": [{"label": "Night Owl", "uiTheme": "vs-dark", "path": "./owl.json"}]}}"#,
        );
        write(
            ".vscode/extensions/sdras.night-owl-2.0.1/owl.json",
            r##"{"colors": {"editor.background": "#011627", "editor.foreground": "#d6deeb"}}"##,
        );
        write(
            ".vscode/extensions/eamodio.gitlens-15.0.0/package.json",
            "{}",
        );
        write(
            ".vscode/extensions/humao.rest-client-0.25.1/package.json",
            "{}",
        );
        write(".vscode/extensions/vscodevim.vim-1.27.0/package.json", "{}");
        write(".vscode/extensions/extensions.json", "[]");
        write(
            ".config/zed/settings.json",
            r#"{"buffer_font_family": "Zed Plex Mono", "buffer_font_size": 15,
                "buffer_line_height": "comfortable", "tab_size": 3, "format_on_save": "on",
                "theme": {"mode": "system", "light": "One Light", "dark": "Tokyo Night"}}"#,
        );
        write(
            ".config/zed/keymap.json",
            r#"[{"bindings": {"cmd-j": "terminal_panel::ToggleFocus"}}]"#,
        );
        write(
            "Library/Application Support/Zed/extensions/installed/tokyo-night/themes/tokyo-night.json",
            r##"{"themes": [{"name": "Tokyo Night", "appearance": "dark",
                 "style": {"background": "#1a1b26", "text": "#a9b1d6"}}]}"##,
        );
        write(
            "Library/Application Support/JetBrains/WebStorm2024.3/options/editor-font.xml",
            "<application></application>",
        );
        write(
            "Library/Application Support/JetBrains/WebStorm2025.1/options/editor-font.xml",
            r#"<application><component name="DefaultFont">
                <option name="FONT_SIZE" value="13" />
                <option name="FONT_FAMILY" value="JetBrains Mono" />
               </component></application>"#,
        );
        write(
            "Library/Application Support/JetBrains/WebStorm2025.1/keymaps/Mine.xml",
            r#"<keymap version="1" name="Mine" parent="macOS">
                <action id="GotoFile"><keyboard-shortcut first-keystroke="meta P" /></action>
               </keymap>"#,
        );
        write(
            "Library/Application Support/JetBrains/consentOptions/accepted",
            "",
        );
        Roots {
            home,
            bundled: Vec::new(),
        }
    }

    fn values(plan: &Plan) -> Vec<(&str, String)> {
        plan.settings.iter().map(|s| (s.key, s.shown())).collect()
    }

    #[test]
    fn finds_editors_and_what_each_has() {
        let roots = home("sources");
        let sources = detect(&roots);
        let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
        // No Cursor here; the older WebStorm is not offered.
        assert_eq!(names, ["VS Code", "Zed", "WebStorm 2025.1"]);

        let code = plan(&sources[0], &roots, true);
        assert_eq!(
            values(&code),
            [
                ("buffer_font_family", "Fira Code".to_string()),
                ("buffer_font_size", "14.0".into()),
                ("buffer_line_height", "21.0".into()),
                ("indent_size", "2".into()),
                ("format_on_save", "on".into()),
            ]
        );
        assert_eq!(code.theme_name.as_deref(), Some("Night Owl"));
        assert_eq!(code.theme.as_ref().unwrap().colors["bg"], "#011627");
        assert_eq!(code.preset_name, Some("VS Code keys"));
        assert!(code.preset.iter().any(|b| b.keys == "cmd-k cmd-s"));
        assert_eq!(code.custom.bindings.len(), 1);
        assert_eq!(code.custom.skipped.len(), 1);
        let hints: Vec<&str> = code.hints.iter().map(|h| h.extension.as_str()).collect();
        assert_eq!(hints, ["eamodio.gitlens", "humao.rest-client"]);

        let zed = plan(&sources[1], &roots, true);
        // Zed's own font and an indent Solder does not have stay behind.
        assert_eq!(
            values(&zed),
            [
                ("buffer_font_size", "15.0".to_string()),
                ("buffer_line_height", "24.0".into()),
                ("format_on_save", "on".into()),
            ]
        );
        assert_eq!(zed.theme_name.as_deref(), Some("Tokyo Night"));
        assert_eq!(zed.theme.as_ref().unwrap().colors["fg"], "#a9b1d6");
        assert_eq!(zed.preset_name, None);
        assert_eq!(zed.custom.bindings[0].action, "workspace::ToggleTerminal");

        let storm = plan(&sources[2], &roots, false);
        assert_eq!(
            values(&storm),
            [
                ("buffer_font_family", "JetBrains Mono".to_string()),
                ("buffer_font_size", "13.0".into()),
            ]
        );
        assert_eq!(storm.theme_name, None);
        assert_eq!(storm.preset_name, Some("JetBrains keys"));
        assert!(storm.preset.iter().any(|b| b.keys == "ctrl-shift-n"));
        assert_eq!(storm.custom.bindings[0].keys, "cmd-p");
        let _ = std::fs::remove_dir_all(roots.home);
    }

    #[test]
    fn nothing_installed_offers_nothing() {
        let home = std::env::temp_dir().join(format!("solder-import-empty-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        assert!(detect(&Roots::system(home.clone())).is_empty());
        let _ = std::fs::remove_dir_all(home);
    }
}
