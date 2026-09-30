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

use crate::theme::{CODE_FONT, Theme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `system`, `dark` or `light`.
    pub theme: ThemeMode,
    pub buffer_font_family: String,
    pub buffer_font_size: f32,
    pub buffer_line_height: f32,
    /// Indent width for files whose indentation cannot be detected.
    pub indent_size: usize,
    /// Latency, frame time and memory in the status bar.
    pub show_performance_hud: bool,
    /// Per-server overrides, keyed by server name (`rust-analyzer`,
    /// `typescript-language-server`, ...).
    pub language_servers: BTreeMap<String, ServerOverride>,
    /// Format with the language server before every save.
    pub format_on_save: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerOverride {
    /// Program to run instead of the one found on `PATH`.
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub disabled: bool,
    pub initialization_options: Option<serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            buffer_font_family: CODE_FONT.to_string(),
            buffer_font_size: 13.,
            buffer_line_height: 20.,
            indent_size: 4,
            show_performance_hud: true,
            language_servers: BTreeMap::new(),
            format_on_save: false,
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

    pub fn indent_unit(&self) -> &'static str {
        match self.indent_size {
            2 => "  ",
            8 => "        ",
            _ => "    ",
        }
    }

    pub fn theme(&self, appearance: gpui::WindowAppearance) -> Theme {
        match self.theme {
            ThemeMode::System => Theme::for_appearance(appearance),
            ThemeMode::Dark => Theme::dark(),
            ThemeMode::Light => Theme::light(),
        }
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

/// Drops `//` line comments so the files can be annotated (JSON with comments).
fn strip_comments(source: &str) -> String {
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
/// `[name, arguments]` for actions that take arguments.
fn parse_keymap(source: &str, cx: &App) -> (Vec<KeyBinding>, Vec<String>) {
    let stripped = strip_comments(source);
    if stripped.trim().is_empty() {
        return (Vec::new(), Vec::new());
    }
    let sections: Vec<KeymapSection> = match serde_json::from_str(&stripped) {
        Ok(s) => s,
        Err(e) => return (Vec::new(), vec![format!("keymap.json: {e}")]),
    };
    let mut bindings = Vec::new();
    let mut errors = Vec::new();
    for section in sections {
        let predicate = match section.context.as_deref() {
            Some(ctx) => match gpui::KeyBindingContextPredicate::parse(ctx) {
                Ok(p) => Some(Rc::new(p)),
                Err(e) => {
                    errors.push(format!("keymap.json: context \u{201c}{ctx}\u{201d}: {e}"));
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
                        errors.push(format!("keymap.json: {keys}: expected [\"action\", args]"));
                        continue;
                    }
                },
                _ => {
                    errors.push(format!("keymap.json: {keys}: expected an action name"));
                    continue;
                }
            };
            let action = match cx.build_action(&name, args) {
                Ok(a) => a,
                Err(e) => {
                    errors.push(format!("keymap.json: {keys}: {e}"));
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
                Err(e) => errors.push(format!("keymap.json: {keys}: {e}")),
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
    crate::terminal::bind_keys(cx);
    crate::git_panel::bind_keys(cx);
    crate::file_diff::bind_keys(cx);
    crate::results::bind_keys(cx);
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Loads both files and applies them. Safe to call again on change.
pub fn reload(cx: &mut App) {
    let mut errors = Vec::new();
    let settings = match parse_settings(&read(&settings_path())) {
        Ok(s) => s,
        Err(e) => {
            errors.push(e);
            // Keep the last good settings rather than resetting everything.
            cx.try_global::<Settings>().cloned().unwrap_or_default()
        }
    };
    cx.global_mut::<crate::perf::Perf>().hud_visible = settings.show_performance_hud;
    cx.set_global(settings);

    cx.clear_key_bindings();
    bind_defaults(cx);
    let (bindings, keymap_errors) = parse_keymap(&read(&keymap_path()), cx);
    cx.bind_keys(bindings);
    errors.extend(keymap_errors);
    for e in &errors {
        eprintln!("{e}");
    }
    cx.set_global(ConfigErrors(errors));
    cx.refresh_windows();
}

/// Applies config changes as soon as the files are saved.
pub fn watch(cx: &mut App) {
    let dir = config_dir();
    if std::fs::create_dir_all(&dir).is_err() {
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
    if watcher.watch(&dir, RecursiveMode::NonRecursive).is_err() {
        return;
    }
    cx.spawn(async move |cx| {
        let _watcher = watcher;
        while rx.next().await.is_some() {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            while rx.try_recv().is_ok() {}
            if cx.update(reload).is_err() {
                break;
            }
        }
    })
    .detach();
}

pub const DEFAULT_KEYMAP: &str = r#"// Key bindings added here apply on top of the defaults.
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
    }

    #[test]
    fn default_file_round_trips() {
        assert_eq!(
            parse_settings(&default_settings_file()).unwrap(),
            Settings::default()
        );
    }
}
