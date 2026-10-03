//! Applies what was picked in the Import window to Solder's config folder.
//!
//! Imports add: a setting the user already changed is not offered, the
//! theme is a file of its own in `themes`, and key bindings go to
//! `keymap-imported.json`, under the user's `keymap.json`. The user's
//! `settings.json` is edited key by key, so its comments stay.

use std::path::{Path, PathBuf};

use import::{Binding, Roots, Setting, ThemeFile, jsonc};
use serde_json::Value;

use crate::settings::{self, Settings};

/// Left in the config folder once the import has been offered.
const OFFERED: &str = ".import-offered";

/// What to bring over from one editor.
#[derive(Clone, Debug, Default)]
pub struct Choice {
    /// The editor's name, for the note in the keymap file.
    pub source: String,
    pub settings: Vec<Setting>,
    pub theme: Option<ThemeFile>,
    pub bindings: Vec<Binding>,
}

/// The user's home folder and the editors' installations.
pub fn system_roots() -> Roots {
    Roots::system(dirs::home_dir().unwrap_or_default())
}

/// The values in the user's `settings.json` that differ from the defaults,
/// by key: what an import must leave alone. Blocking.
pub fn changed_settings(dir: &Path) -> serde_json::Map<String, Value> {
    let text = std::fs::read_to_string(dir.join("settings.json")).unwrap_or_default();
    let (Ok(Value::Object(mut current)), Ok(Value::Object(defaults))) = (
        jsonc::parse(&text),
        serde_json::to_value(Settings::default()),
    ) else {
        return Default::default();
    };
    current.retain(|key, value| defaults.get(key) != Some(value));
    current
}

/// Whether to offer the import now: Solder has no settings yet, it was not
/// offered before, and another editor is here. Remembers the offer.
/// Blocking.
pub fn first_launch_offer(dir: &Path, roots: &Roots) -> bool {
    if dir.join("settings.json").exists() || dir.join(OFFERED).exists() {
        return false;
    }
    if import::detect(roots).is_empty() {
        return false;
    }
    std::fs::create_dir_all(dir).is_ok() && std::fs::write(dir.join(OFFERED), "").is_ok()
}

/// Writes the choice into the config folder `dir` and says what was done.
/// Blocking.
pub fn apply(dir: &Path, choice: &Choice) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut done = Vec::new();

    let path = dir.join("settings.json");
    let before =
        std::fs::read_to_string(&path).unwrap_or_else(|_| settings::default_settings_file());
    let mut text = before.clone();
    for setting in &choice.settings {
        text = jsonc::set_key(&text, setting.key, &setting.value);
    }
    if let Some(theme) = &choice.theme {
        let file = settings::theme_path(dir, &theme.name);
        write(
            &file,
            &serde_json::to_string_pretty(theme).map_err(|e| e.to_string())?,
        )?;
        text = jsonc::set_key(&text, "theme", &Value::from(theme.name.clone()));
        done.push(format!("the theme {}", theme.name));
    }
    if text != before {
        // Never leave a file the editor cannot read.
        settings::parse_settings(&text).map_err(|e| format!("Not applied: {e}"))?;
        write(&path, &text)?;
    }
    match choice.settings.len() {
        0 => {}
        1 => done.insert(0, "1 setting".into()),
        n => done.insert(0, format!("{n} settings")),
    }

    if !choice.bindings.is_empty() {
        let keymap = format!(
            "// Key bindings imported from {}. Your own, in keymap.json, win over these.\n\
             // Importing again replaces this file; delete it to drop them.\n{}\n",
            choice.source,
            import::keymap::to_json(&choice.bindings)
        );
        write(&dir.join(settings::IMPORTED_KEYMAP), &keymap)?;
        done.push(match choice.bindings.len() {
            1 => "1 key binding".into(),
            n => format!("{n} key bindings"),
        });
    }
    Ok(done)
}

fn write(path: &PathBuf, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setting(key: &'static str, value: Value) -> Setting {
        Setting {
            key,
            label: "",
            value,
        }
    }

    #[test]
    fn an_import_adds_to_the_config_and_keeps_what_is_there() {
        let dir = db::testing::dir("import-apply");
        std::fs::write(
            dir.join("settings.json"),
            "// Mine.\n{\n  \"buffer_font_size\": 16, // big\n  \"indent_size\": 4\n}\n",
        )
        .unwrap();
        // Only what differs from the defaults counts as the user's.
        let changed = changed_settings(&dir);
        assert_eq!(changed.keys().collect::<Vec<_>>(), ["buffer_font_size"]);

        let theme = ThemeFile {
            name: "Night Owl".into(),
            appearance: "dark".into(),
            colors: [("bg".to_string(), "#011627".to_string())].into(),
            syntax: Default::default(),
        };
        let done = apply(
            &dir,
            &Choice {
                source: "VS Code".into(),
                settings: vec![
                    setting("format_on_save", json!(true)),
                    setting("indent_size", json!(2)),
                ],
                theme: Some(theme.clone()),
                bindings: vec![Binding {
                    keys: "cmd-j".into(),
                    action: "workspace::ToggleTerminal",
                    context: None,
                }],
            },
        )
        .unwrap();
        assert_eq!(done, ["2 settings", "the theme Night Owl", "1 key binding"]);

        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(text.starts_with("// Mine.\n{"), "{text}");
        assert!(text.contains("\"buffer_font_size\": 16, // big"), "{text}");
        let settings = settings::parse_settings(&text).unwrap();
        assert_eq!(
            settings.theme,
            settings::ThemeMode::Named("Night Owl".into())
        );
        assert!(settings.format_on_save);
        assert_eq!((settings.indent_size, settings.buffer_font_size), (2, 16.));
        let saved: ThemeFile = serde_json::from_str(
            &std::fs::read_to_string(dir.join("themes/night-owl.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved, theme);
        let keymap = std::fs::read_to_string(dir.join("keymap-imported.json")).unwrap();
        assert!(keymap.starts_with("// Key bindings imported from VS Code."));
        assert!(keymap.contains("\"cmd-j\": \"workspace::ToggleTerminal\""));
        // The user's own keymap was not made or touched.
        assert!(!dir.join("keymap.json").exists());

        // Nothing picked changes nothing.
        let before = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(apply(&dir, &Choice::default()).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.join("settings.json")).unwrap(),
            before
        );
    }

    #[test]
    fn the_import_is_offered_once_and_only_on_a_first_launch() {
        let home = db::testing::dir("import-offer-home");
        let config = home.join(".config/solder");
        let roots = Roots {
            home: home.clone(),
            bundled: Vec::new(),
        };
        // No other editor: nothing to offer, and nothing remembered.
        assert!(!first_launch_offer(&config, &roots));
        assert!(!config.join(OFFERED).exists());

        std::fs::create_dir_all(home.join(".config/zed")).unwrap();
        std::fs::write(home.join(".config/zed/settings.json"), "{}").unwrap();
        assert!(first_launch_offer(&config, &roots));
        assert!(!first_launch_offer(&config, &roots));

        // Someone who already has settings is not asked.
        let settled = home.join("settled");
        std::fs::create_dir_all(&settled).unwrap();
        std::fs::write(settled.join("settings.json"), "{}").unwrap();
        assert!(!first_launch_offer(&settled, &roots));
    }
}
