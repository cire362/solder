//! User settings and key bindings from `~/.config/solder/`. Both files reload
//! as soon as they are saved.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use futures::StreamExt;
use gpui::{App, Font, Global, KeyBinding, Pixels, px};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};

use crate::theme::{CODE_FONT, Theme, Tokens, UI_FONT, UI_FONT_PX};

/// `system`, `dark`, `light`, or the name of a theme in the `themes`
/// folder next to `settings.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    #[default]
    System,
    Dark,
    Light,
    Named(String),
}

impl Serialize for ThemeMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::System => "system",
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Named(name) => name,
        })
    }
}

impl<'de> Deserialize<'de> for ThemeMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(match name.as_str() {
            "system" => Self::System,
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => Self::Named(name),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `system`, `dark`, `light`, or a theme in the `themes` folder.
    pub theme: ThemeMode,
    /// The named theme, read when the settings are.
    #[serde(skip)]
    pub custom_theme: Option<Theme>,
    /// Tokens set over the theme in use, whichever it is.
    pub theme_overrides: ThemeOverrides,
    pub buffer_font_family: String,
    pub buffer_font_size: f32,
    pub buffer_line_height: f32,
    /// The font of everything that is not code: panels, tabs, the bars.
    pub ui_font_family: String,
    /// Its size. The room around the text grows and shrinks with it.
    pub ui_font_size: f32,
    /// How tall the rows of lists are: `compact`, `default`, `comfortable`.
    pub ui_density: Density,
    /// Default, VS Code, JetBrains, or a file in `keymaps`.
    pub key_layout: String,
    /// Indent width for files whose indentation cannot be detected.
    pub indent_size: usize,
    /// Latency, frame time and memory in the status bar.
    pub show_performance_hud: bool,
    /// Per-server overrides, keyed by server name (`rust-analyzer`,
    /// `typescript-language-server`, ...).
    pub language_servers: BTreeMap<String, ServerOverride>,
    /// What is set for one language, by the language's name.
    pub languages: BTreeMap<String, LanguageSettings>,
    /// Format with the language server before every save.
    pub format_on_save: bool,
    /// What a language server puts into lines: the types it worked out,
    /// the names of parameters.
    pub inlay_hints: bool,
    /// Colors from the language server over the grammar's, where it says
    /// what a word is.
    pub semantic_highlighting: bool,
    /// What a language server offers to do with a line, above it.
    pub code_lens: bool,
    /// A thin line down each level of indentation.
    pub indent_guides: bool,
    /// The icon theme of an installed extension, by name: pictures next
    /// to file names in the tree and on tabs. None by default.
    pub icon_theme: Option<String>,
    /// Model Context Protocol servers the agent may use, by a name of the
    /// user's choosing. Started when an agent task begins.
    pub context_servers: BTreeMap<String, ContextServer>,
    /// What the file sets that is none of the above: the settings of
    /// extensions, under the names their manifests declare, written with
    /// the dots (`"prettier.tabWidth": 2`) or as objects inside objects.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

/// One context server: the program to start and what to start it with,
/// or the address of one that is somewhere else.
/// The command is written as Zed writes it, in either of its two forms:
/// `"command": "npx", "args": [...]`, or
/// `"command": { "path": "npx", "args": [...], "env": {...} }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextServer {
    pub command: Option<ServerCommand>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// A server reached over HTTP, in place of a command: its address,
    /// and what goes with every message to it (a key, mostly).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// For a server an extension brings, under the extension's name for
    /// it: what the extension reads to start it (a database's address, a
    /// token). The extension says which it needs.
    pub settings: Option<serde_json::Value>,
    /// `false` keeps it in the file and out of use.
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerCommand {
    Program(String),
    Table {
        path: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
}

impl Default for ContextServer {
    fn default() -> Self {
        Self {
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            settings: None,
            enabled: true,
        }
    }
}

impl ContextServer {
    /// The address of a server that is somewhere else. A command wins:
    /// an entry with both starts the program.
    pub fn address(&self) -> Option<&str> {
        let url = self.url.as_deref().map(str::trim)?;
        (self.program().is_none() && !url.is_empty()).then_some(url)
    }

    pub fn program(&self) -> Option<String> {
        match self.command.as_ref()? {
            ServerCommand::Program(program) | ServerCommand::Table { path: program, .. } => {
                Some(program.clone()).filter(|program| !program.trim().is_empty())
            }
        }
    }

    pub fn arguments(&self) -> Vec<String> {
        match &self.command {
            Some(ServerCommand::Table { args, .. }) if self.args.is_empty() => args.clone(),
            _ => self.args.clone(),
        }
    }

    pub fn variables(&self) -> BTreeMap<String, String> {
        let mut variables = match &self.command {
            Some(ServerCommand::Table { env, .. }) => env.clone(),
            _ => BTreeMap::new(),
        };
        variables.extend(self.env.clone());
        variables
    }
}

/// The settings of one language.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguageSettings {
    /// Which of the language's servers start, the first being the one
    /// asked what only one can answer. Written as Zed writes it: a name,
    /// `!name` for one that does not start, and `...` for all the others.
    /// Without `...` only the ones named start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_servers: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerOverride {
    /// Program to run instead of the one found on `PATH`.
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub disabled: bool,
    pub initialization_options: Option<serde_json::Value>,
    /// What the server is told its settings are, and answered when it asks.
    pub settings: Option<serde_json::Value>,
}

/// Tokens of a theme set in `settings.json`, as a theme file names them:
/// `"accent": "#ff8800"`, and `syntax` and `terminal` for theirs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeOverrides {
    #[serde(skip_serializing_if = "Tokens::is_empty")]
    pub syntax: Tokens,
    #[serde(skip_serializing_if = "Tokens::is_empty")]
    pub terminal: Tokens,
    /// `control_radius`, `border_width` and the like, in pixels.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub shapes: BTreeMap<String, f32>,
    #[serde(flatten)]
    pub colors: Tokens,
}

impl ThemeOverrides {
    /// What is wrong in them: a name that is no token, a value that is
    /// no color. Such a line changes nothing, which is worth saying.
    pub fn mistakes(&self) -> Vec<String> {
        use import::theme::{COLORS, Rgba, SYNTAX, TERMINAL};
        let mut mistakes = Vec::new();
        for (tokens, known, of) in [
            (&self.colors, COLORS, ""),
            (&self.syntax, SYNTAX, "syntax."),
            (&self.terminal, TERMINAL, "terminal."),
        ] {
            for (name, value) in tokens {
                if !known.contains(&name.as_str()) {
                    mistakes.push(format!(
                        "settings.json: theme_overrides: no token \u{201c}{of}{name}\u{201d}"
                    ));
                } else if Rgba::parse(value).is_none() {
                    mistakes.push(format!(
                        "settings.json: theme_overrides: {of}{name}: \u{201c}{value}\u{201d} is not a color"
                    ));
                }
            }
        }
        for name in self.shapes.keys() {
            if !import::theme::SHAPES.contains(&name.as_str()) {
                mistakes.push(format!(
                    "settings.json: theme_overrides: no shape \u{201c}{name}\u{201d}"
                ));
            }
        }
        mistakes
    }
}

/// How tall the rows of lists are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    #[default]
    #[serde(rename = "default")]
    Standard,
    Comfortable,
}

/// The sizes the interface's text may have. Below the first it cannot be
/// read; above the second a tab no longer holds its name.
const UI_FONT_LEAST: f32 = 9.;
const UI_FONT_MOST: f32 = 18.;

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            custom_theme: None,
            theme_overrides: ThemeOverrides::default(),
            buffer_font_family: CODE_FONT.to_string(),
            buffer_font_size: 13.,
            buffer_line_height: 20.,
            ui_font_family: UI_FONT.to_string(),
            ui_font_size: UI_FONT_PX,
            ui_density: Density::Standard,
            key_layout: crate::key_layout::DEFAULT.into(),
            indent_size: 4,
            show_performance_hud: true,
            language_servers: BTreeMap::new(),
            languages: BTreeMap::new(),
            format_on_save: false,
            inlay_hints: true,
            semantic_highlighting: true,
            code_lens: true,
            indent_guides: true,
            icon_theme: None,
            context_servers: BTreeMap::new(),
            other: BTreeMap::new(),
        }
    }
}

impl Global for Settings {}

impl Settings {
    pub fn get(cx: &App) -> &Settings {
        cx.global::<Settings>()
    }

    pub fn buffer_font(&self) -> Font {
        gpui::font(self.buffer_font_family.clone())
    }

    pub fn buffer_font_size(&self) -> Pixels {
        px(self.buffer_font_size.clamp(6., 72.))
    }

    pub fn line_height(&self) -> Pixels {
        px(self.buffer_line_height.max(self.buffer_font_size * 1.1))
    }

    pub fn ui_font(&self) -> gpui::SharedString {
        self.ui_font_family.clone().into()
    }

    /// The interface's text size, kept to what its rows can hold.
    fn ui_scale(&self) -> f32 {
        let size = if self.ui_font_size.is_finite() {
            self.ui_font_size
        } else {
            UI_FONT_PX
        };
        size.clamp(UI_FONT_LEAST, UI_FONT_MOST) / UI_FONT_PX
    }

    /// What a rem is in a window: 16 pixels as it comes. Every text size
    /// and most of the spacing of the interface is in rems.
    pub fn rem_size(&self) -> Pixels {
        px(16. * self.ui_scale())
    }

    /// How much taller than it comes a row of a list is: by the density,
    /// and by the text when that is larger. Smaller text leaves rows as
    /// they are; `compact` is what lowers them.
    pub fn row_scale(&self) -> f32 {
        let density = match self.ui_density {
            Density::Compact => 0.85,
            Density::Standard => 1.,
            Density::Comfortable => 1.25,
        };
        density * self.ui_scale().max(1.)
    }

    pub fn indent_unit(&self) -> &'static str {
        match self.indent_size {
            2 => "  ",
            8 => "        ",
            _ => "    ",
        }
    }

    pub fn theme(&self, appearance: gpui::WindowAppearance) -> Theme {
        let mut theme = match (&self.theme, &self.custom_theme) {
            (ThemeMode::Named(_), Some(theme)) => theme.clone(),
            (ThemeMode::Dark, _) => Theme::dark(),
            (ThemeMode::Light, _) => Theme::light(),
            // The system's, also for a named theme whose file is gone.
            _ => Theme::for_appearance(appearance),
        };
        let over = &self.theme_overrides;
        theme.lay(&over.colors, &over.syntax, &over.terminal);
        theme.shape.lay(&over.shapes);
        theme
    }
}

/// Problems with the user's config files, shown in the status bar.
#[derive(Default)]
pub struct ConfigErrors(pub Vec<String>);

impl Global for ConfigErrors {}

pub fn config_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config/solder")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn keymap_path() -> PathBuf {
    config_dir().join("keymap.json")
}

/// Bindings brought over from another editor. Loaded under `keymap.json`,
/// so the user's own bindings win.
pub const IMPORTED_KEYMAP: &str = "keymap-imported.json";

/// Where the theme called `name` is kept: `themes/night-owl.json`.
pub fn theme_path(dir: &Path, name: &str) -> PathBuf {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    dir.join("themes")
        .join(format!("{}.json", slug.trim_matches('-')))
}

/// Reads a named theme; blocking, on a small file.
fn read_theme(dir: &Path, name: &str) -> Result<Theme, String> {
    let path = theme_path(dir, name);
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!("settings.json: no theme \u{201c}{name}\u{201d} in the themes folder")
    })?;
    let file: import::ThemeFile = serde_json::from_str(&strip_comments(&text))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Theme::from_file(&file))
}

/// Drops `//` line comments so the files can be annotated (JSON with comments).
pub(crate) fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let mut in_string = false;
        let mut escaped = false;
        let mut cut = line.len();
        let bytes = line.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                _ if escaped => escaped = false,
                b'\\' if in_string => escaped = true,
                b'"' => in_string = !in_string,
                b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => {
                    cut = i;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

pub fn parse_settings(source: &str) -> Result<Settings, String> {
    let stripped = strip_comments(source);
    if stripped.trim().is_empty() {
        return Ok(Settings::default());
    }
    serde_json::from_str(&stripped).map_err(|e| format!("settings.json: {e}"))
}

#[derive(Deserialize)]
struct KeymapSection {
    context: Option<String>,
    bindings: serde_json::Map<String, serde_json::Value>,
}

/// Builds user key bindings. Each value is an action name, or
/// `[name, arguments]` for actions that take arguments. `file` names the
/// file in what is reported.
pub(crate) fn parse_keymap(file: &str, source: &str, cx: &App) -> (Vec<KeyBinding>, Vec<String>) {
    let stripped = strip_comments(source);
    if stripped.trim().is_empty() {
        return (Vec::new(), Vec::new());
    }
    let sections: Vec<KeymapSection> = match serde_json::from_str(&stripped) {
        Ok(s) => s,
        Err(e) => return (Vec::new(), vec![format!("{file}: {e}")]),
    };
    let mut bindings = Vec::new();
    let mut errors = Vec::new();
    for section in sections {
        let predicate = match section.context.as_deref() {
            Some(ctx) => match gpui::KeyBindingContextPredicate::parse(ctx) {
                Ok(p) => Some(Rc::new(p)),
                Err(e) => {
                    errors.push(format!("{file}: context \u{201c}{ctx}\u{201d}: {e}"));
                    continue;
                }
            },
            None => None,
        };
        for (keys, value) in section.bindings {
            let (name, args) = match &value {
                serde_json::Value::String(name) => (name.clone(), None),
                serde_json::Value::Array(items) => match items.as_slice() {
                    [serde_json::Value::String(name), args] => (name.clone(), Some(args.clone())),
                    _ => {
                        errors.push(format!("{file}: {keys}: expected [\"action\", args]"));
                        continue;
                    }
                },
                _ => {
                    errors.push(format!("{file}: {keys}: expected an action name"));
                    continue;
                }
            };
            let action = match cx.build_action(&name, args) {
                Ok(a) => a,
                Err(e) => {
                    errors.push(format!("{file}: {keys}: {e}"));
                    continue;
                }
            };
            match KeyBinding::load(
                &keys,
                action,
                predicate.clone(),
                false,
                None,
                cx.keyboard_mapper().as_ref(),
            ) {
                Ok(b) => bindings.push(b),
                Err(e) => errors.push(format!("{file}: {keys}: {e}")),
            }
        }
    }
    (bindings, errors)
}

/// Default bindings from every module. Tests call this too, so a new
/// module's bindings cannot be missing from them.
pub fn bind_defaults(cx: &mut App) {
    crate::editor::bind_keys(cx);
    crate::workspace::bind_keys(cx);
    crate::picker::bind_keys(cx);
    crate::buffer_search::bind_keys(cx);
    crate::project_search::bind_keys(cx);
    crate::project_panel::bind_keys(cx);
    crate::extension_views::bind_keys(cx);
    crate::terminal::bind_keys(cx);
    crate::git_panel::bind_keys(cx);
    crate::file_diff::bind_keys(cx);
    crate::results::bind_keys(cx);
    crate::plugins_view::bind_keys(cx);
    crate::extensions_panel::bind_keys(cx);
    crate::import_view::bind_keys(cx);
    crate::structure::bind_keys(cx);
    crate::erd_view::bind_keys(cx);
    crate::ai_panel::bind_keys(cx);
    crate::chat_panel::bind_keys(cx);
    crate::agent_panel::bind_keys(cx);
    crate::debug_panel::bind_keys(cx);
    crate::inline_edit::bind_keys(cx);
    // After the editor's: in a cell, two of its keys run the cell.
    crate::notebook::bind_keys(cx);
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[derive(Default)]
struct KeyFiles {
    imported: Vec<KeyBinding>,
    personal: Vec<KeyBinding>,
}
impl Global for KeyFiles {}

/// The keys installed extensions bind to their commands.
#[derive(Default)]
struct ExtensionKeys(Vec<KeyBinding>);
impl Global for ExtensionKeys {}

/// A named set arrives from the background. Use the newest personal and
/// imported files, rather than a snapshot from when its read began. What
/// extensions bind is over the layout and under the user's own keys, as
/// in VS Code.
pub(crate) fn bind_key_files(selected: Vec<KeyBinding>, cx: &mut App) {
    let files = cx.default_global::<KeyFiles>();
    let imported = files.imported.clone();
    let personal = files.personal.clone();
    let extensions = cx.default_global::<ExtensionKeys>().0.clone();
    cx.clear_key_bindings();
    bind_defaults(cx);
    cx.bind_keys(imported);
    cx.bind_keys(selected);
    cx.bind_keys(extensions);
    cx.bind_keys(personal);
}

/// The keys extensions bind changed: `source` is all of them, written as
/// a keymap file is. Gives what in it could not be bound.
pub fn set_extension_keys(source: &str, cx: &mut App) -> Vec<String> {
    let (keys, errors) = parse_keymap("extensions", source, cx);
    cx.set_global(ExtensionKeys(keys));
    bind_key_files(crate::key_layout::kept(cx), cx);
    errors
}

/// Loads the config files and applies them. Safe to call again on change.
pub fn reload(cx: &mut App) {
    reload_from(&config_dir(), cx);
}

/// `reload`, from the config folder given.
pub fn reload_from(dir: &Path, cx: &mut App) {
    // A selection writes the setting and its file together. Watching an
    // intermediate save must not apply just half of that change.
    if crate::key_layout::pending(cx) {
        return;
    }
    let mut errors = Vec::new();
    let mut settings = match parse_settings(&read(&dir.join("settings.json"))) {
        Ok(s) => s,
        Err(e) => {
            errors.push(e);
            // Keep the last good settings rather than resetting everything.
            cx.try_global::<Settings>().cloned().unwrap_or_default()
        }
    };
    if let ThemeMode::Named(name) = &settings.theme {
        match read_theme(dir, name) {
            Ok(theme) => settings.custom_theme = Some(theme),
            Err(e) => errors.push(e),
        }
    }
    errors.extend(settings.theme_overrides.mistakes());
    cx.global_mut::<crate::perf::Perf>().hud_visible = settings.show_performance_hud;
    let key_layout = settings.key_layout.clone();
    cx.set_global(settings);

    // An explicit choice wins over an older import. Personal bindings
    // remain above both, so choosing a set never discards them.
    let (imported, keymap_errors) =
        parse_keymap(IMPORTED_KEYMAP, &read(&dir.join(IMPORTED_KEYMAP)), cx);
    errors.extend(keymap_errors);
    let (personal, keymap_errors) =
        parse_keymap("keymap.json", &read(&dir.join("keymap.json")), cx);
    errors.extend(keymap_errors);
    cx.set_global(KeyFiles { imported, personal });
    let (bindings, keymap_errors) = crate::key_layout::reload_from(dir, &key_layout, cx);
    bind_key_files(bindings, cx);
    errors.extend(keymap_errors);
    errors.extend(crate::layout::reload_from(dir, cx));
    for e in &errors {
        eprintln!("{e}");
    }
    cx.set_global(ConfigErrors(errors));
    cx.refresh_windows();
}

/// Applies config changes as soon as the files are saved.
pub fn watch(cx: &mut App) {
    watch_dir(config_dir(), cx);
}

/// Reads the config files in `dir` again whenever one of them is saved,
/// a theme, named layout or key layout in its folder too.
pub fn watch_dir(dir: PathBuf, cx: &mut App) {
    // The folder of themes is watched from the start, so that the first
    // theme put there is seen.
    let themes = dir.join("themes");
    let layouts = dir.join("layouts");
    let keymaps = dir.join("keymaps");
    if [&themes, &layouts, &keymaps]
        .into_iter()
        .any(|dir| std::fs::create_dir_all(dir).is_err())
    {
        return;
    }
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
    let Ok(mut watcher) = notify::recommended_watcher(move |e: notify::Result<notify::Event>| {
        if e.is_ok() {
            tx.unbounded_send(()).ok();
        }
    }) else {
        return;
    };
    if watcher.watch(&dir, RecursiveMode::NonRecursive).is_err()
        || watcher.watch(&themes, RecursiveMode::NonRecursive).is_err()
        || watcher
            .watch(&layouts, RecursiveMode::NonRecursive)
            .is_err()
        || watcher
            .watch(&keymaps, RecursiveMode::NonRecursive)
            .is_err()
    {
        return;
    }
    cx.spawn(async move |cx| {
        let _watcher = watcher;
        while rx.next().await.is_some() {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            while rx.try_recv().is_ok() {}
            if cx.update(|cx| reload_from(&dir, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
}

pub const DEFAULT_KEYMAP: &str = r#"// Key bindings added here apply on top of the selected key layout.
// Run "Workspace: Toggle command palette" to see action names.
[
  {
    "context": "Editor && mode == full",
    "bindings": {
      // "cmd-shift-k": "editor::DuplicateLine"
    }
  }
]
"#;

pub fn default_settings_file() -> String {
    let json = serde_json::to_string_pretty(&Settings::default()).unwrap_or_default();
    format!("// Changes apply as soon as you save.\n{json}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_parse_with_comments_and_defaults() {
        let s = parse_settings(
            "// hi\n{ \"theme\": \"dark\", // trailing\n \"buffer_font_size\": 15 }",
        )
        .unwrap();
        assert_eq!(s.theme, ThemeMode::Dark);
        assert_eq!(s.buffer_font_size, 15.);
        assert_eq!(s.indent_size, 4);
        assert!(parse_settings("{ \"theme\": 3 }").is_err());
        assert_eq!(parse_settings("").unwrap(), Settings::default());
        let url = parse_settings("{ \"buffer_font_family\": \"a//b\" }").unwrap();
        assert_eq!(url.buffer_font_family, "a//b");
        // What the file sets that Solder has no setting for is kept, for
        // the extension that declared it; what Solder has is not.
        let kept = parse_settings(
            r#"{ "indent_size": 2, "acme.lint.level": 3, "acme": { "format": false } }"#,
        )
        .unwrap();
        assert_eq!(kept.indent_size, 2);
        let keys: Vec<&str> = kept.other.keys().map(String::as_str).collect();
        assert_eq!(keys, ["acme", "acme.lint.level"]);
        assert!(s.other.is_empty());
        // Which servers a language starts, as Zed writes it.
        let ruby = parse_settings(
            r#"{ "languages": { "Ruby": { "language_servers": ["ruby-lsp", "!solargraph", "..."] } } }"#,
        )
        .unwrap();
        assert_eq!(
            ruby.languages["Ruby"].language_servers.as_deref(),
            Some(&["ruby-lsp".to_string(), "!solargraph".into(), "...".into()][..])
        );
        assert!(s.languages.is_empty());
        // The interface's text as it comes: a rem is 16 pixels, and rows
        // are as tall as they were written.
        assert_eq!(s.ui_font_family, UI_FONT);
        assert_eq!((s.rem_size(), s.row_scale()), (px(16.), 1.));
        // Larger text, and the rem and the rows grow with it; a size no
        // row can hold is brought to the nearest that fits.
        let large = parse_settings(r#"{ "ui_font_size": 15, "ui_font_family": "Inter" }"#).unwrap();
        assert_eq!(large.ui_font(), "Inter");
        assert_eq!(large.rem_size(), px(19.2));
        assert_eq!(large.row_scale(), 1.2);
        let huge = parse_settings(r#"{ "ui_font_size": 400 }"#).unwrap();
        assert_eq!(huge.rem_size(), px(16. * 18. / 12.5));
        // Smaller text leaves the rows alone: `compact` lowers them.
        let small = parse_settings(r#"{ "ui_font_size": 10 }"#).unwrap();
        assert_eq!((small.rem_size(), small.row_scale()), (px(12.8), 1.));
        let compact = parse_settings(r#"{ "ui_density": "compact" }"#).unwrap();
        assert_eq!(compact.row_scale(), 0.85);
        let roomy = parse_settings(r#"{ "ui_density": "comfortable" }"#).unwrap();
        assert_eq!(roomy.row_scale(), 1.25);
        assert_eq!(
            parse_settings(r#"{ "ui_density": "default" }"#).unwrap(),
            Settings::default()
        );
        assert!(parse_settings(r#"{ "ui_density": "airy" }"#).is_err());
        // A context server's command in both of the forms Zed writes it.
        let servers = parse_settings(
            r#"{ "context_servers": {
                "flat": { "command": "npx", "args": ["-y", "pkg"], "env": { "KEY": "1" } },
                "table": { "command": { "path": "uvx", "args": ["tool"], "env": { "A": "b" } }, "enabled": false },
                "bare": {}
            } }"#,
        )
        .unwrap()
        .context_servers;
        assert_eq!(servers["flat"].program().as_deref(), Some("npx"));
        assert_eq!(servers["flat"].arguments(), ["-y", "pkg"]);
        assert_eq!(servers["flat"].variables()["KEY"], "1");
        assert!(servers["flat"].enabled);
        assert_eq!(servers["table"].program().as_deref(), Some("uvx"));
        assert_eq!(servers["table"].arguments(), ["tool"]);
        assert_eq!(servers["table"].variables()["A"], "b");
        assert!(!servers["table"].enabled);
        assert_eq!(servers["bare"].program(), None);
        // One that is somewhere else: an address and what to send with
        // each message. With a command too, the command is what counts.
        let remote = parse_settings(
            r#"{ "context_servers": {
                "issues": { "url": " https://example.com/mcp ", "headers": { "Authorization": "Bearer k" } },
                "both": { "url": "https://example.com/mcp", "command": "npx" }
            } }"#,
        )
        .unwrap()
        .context_servers;
        assert_eq!(remote["issues"].address(), Some("https://example.com/mcp"));
        assert_eq!(remote["issues"].headers["Authorization"], "Bearer k");
        assert_eq!(remote["both"].address(), None);
        assert_eq!(servers["flat"].address(), None);
    }

    #[test]
    fn tokens_are_set_over_the_theme_in_use() {
        use gpui::{Hsla, WindowAppearance::Dark, rgb};
        let settings = parse_settings(
            r##"{
              "theme": "light",
              "theme_overrides": {
                "accent": "#00ff88",
                "syntax": { "comment": "#777777" },
                "terminal": { "bright_red": "#ff0000" }
              }
            }"##,
        )
        .unwrap();
        assert!(settings.theme_overrides.mistakes().is_empty());
        // The theme in use with those three laid over it, and nothing
        // else moved.
        let theme = settings.theme(Dark);
        let light = Theme::light();
        assert_eq!(theme.accent, Hsla::from(rgb(0x00ff88)));
        assert_eq!(theme.syntax.comment, Hsla::from(rgb(0x777777)));
        assert_eq!(theme.terminal[9], Hsla::from(rgb(0xff0000)));
        assert_eq!(
            (theme.bg, theme.syntax.keyword),
            (light.bg, light.syntax.keyword)
        );
        assert_eq!(theme.terminal[1], light.terminal[1]);
        // Over the system's theme and a named one alike.
        let system = parse_settings(r##"{ "theme_overrides": { "bg": "#000000" } }"##).unwrap();
        assert_eq!(system.theme(Dark).bg, Hsla::from(rgb(0x000000)));
        assert_eq!(system.theme(Dark).fg, Theme::dark().fg);

        // A name that is no token and a value that is no color change
        // nothing, and are said.
        let wrong = parse_settings(
            r##"{ "theme_overrides": {
                "acent": "#00ff88", "fg": "red", "syntax": { "keyword": "#12" }, "terminal": { "pink": "#ff00ff" }
            } }"##,
        )
        .unwrap();
        assert_eq!(wrong.theme(Dark), Theme::dark());
        let mistakes = wrong.theme_overrides.mistakes();
        assert_eq!(mistakes.len(), 4, "{mistakes:?}");
        assert!(
            mistakes
                .iter()
                .all(|m| m.starts_with("settings.json: theme_overrides"))
        );
        assert!(mistakes.iter().any(|m| m.contains("acent")));
        assert!(mistakes.iter().any(|m| m.contains("terminal.pink")));
        assert!(
            mistakes
                .iter()
                .any(|m| m.contains("syntax.keyword") && m.contains("not a color"))
        );
        // Nothing set is nothing written.
        assert!(default_settings_file().contains("\"theme_overrides\": {}"));

        // Shapes are set the same way, in pixels, and kept to what a
        // window can draw.
        let shaped = parse_settings(
            r#"{ "theme_overrides": { "shapes": {
                "control_radius": 2, "token_radius": 0, "border_width": 40, "corner": 3
            } } }"#,
        )
        .unwrap();
        let shape = shaped.theme(Dark).shape;
        assert_eq!((shape.control, shape.token), (px(2.), px(0.)));
        assert_eq!(shape.border, px(3.));
        // How much room there is around things is a shape too: a number
        // to multiply by, kept to what still reads.
        assert_eq!(shape.spacing, 1.);
        let airy =
            parse_settings(r#"{ "theme_overrides": { "shapes": { "spacing": 1.25 } } }"#).unwrap();
        assert_eq!(airy.theme(Dark).shape.spacing, 1.25);
        assert!(airy.theme_overrides.mistakes().is_empty());
        let wide =
            parse_settings(r#"{ "theme_overrides": { "shapes": { "spacing": 9 } } }"#).unwrap();
        assert_eq!(wide.theme(Dark).shape.spacing, 1.5);
        assert_eq!(shape.panel, crate::theme::Shapes::default().panel);
        assert_eq!(shaped.theme(Dark).bg, Theme::dark().bg);
        let mistakes = shaped.theme_overrides.mistakes();
        assert_eq!(mistakes.len(), 1, "{mistakes:?}");
        assert!(mistakes[0].contains("no shape") && mistakes[0].contains("corner"));
    }

    #[test]
    fn default_file_round_trips() {
        assert_eq!(
            parse_settings(&default_settings_file()).unwrap(),
            Settings::default()
        );
    }
}
