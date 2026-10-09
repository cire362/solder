//! A chosen set of keys sits between imported keys and the user's own.
//! The file remains editable, and an invalid save keeps the last good set.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use gpui::{App, Global, KeyBinding, Task};

use crate::settings;

pub const DEFAULT: &str = "Default";
pub const VSCODE: &str = "VS Code";
pub const JETBRAINS: &str = "JetBrains";
pub const BUILT_INS: [&str; 3] = [DEFAULT, VSCODE, JETBRAINS];

struct Loaded {
    dir: PathBuf,
    name: String,
    source: String,
}
impl Global for Loaded {}

#[derive(Default)]
struct Writes {
    last: Option<Task<()>>,
    pending: usize,
    error: Option<String>,
}
impl Global for Writes {}

#[derive(Default)]
struct Reads {
    generation: u64,
    last: Option<Task<()>>,
    errors: Vec<String>,
}
impl Global for Reads {}

pub fn pending(cx: &App) -> bool {
    cx.try_global::<Writes>().is_some_and(|w| w.pending > 0)
}

pub fn active(cx: &App) -> &str {
    cx.try_global::<Loaded>().map_or(DEFAULT, |l| &l.name)
}

pub fn folder(cx: &App) -> PathBuf {
    cx.try_global::<Loaded>()
        .map_or_else(settings::config_dir, |l| l.dir.clone())
}

pub fn path(cx: &App) -> Option<PathBuf> {
    (!BUILT_INS.contains(&active(cx))).then(|| named_path(&folder(cx), active(cx)))
}

fn named_path(dir: &Path, name: &str) -> PathBuf {
    dir.join("keymaps").join(format!("{name}.json"))
}

pub fn valid_name(name: &str) -> bool {
    crate::layout::valid_name(name)
        && !BUILT_INS
            .iter()
            .any(|built_in| name.eq_ignore_ascii_case(built_in))
}

pub fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir.join("keymaps"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension()? == "json").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .filter(|name| valid_name(name))
        .collect();
    names.sort();
    names
}

fn source(dir: &Path, name: &str) -> Result<String, String> {
    let mac = cfg!(target_os = "macos");
    Ok(match name {
        DEFAULT => "[]\n".into(),
        VSCODE => import::keymap::to_json(&import::keymap::vscode_preset(mac)),
        JETBRAINS => import::keymap::to_json(&import::keymap::jetbrains_preset(mac)),
        name if valid_name(name) => {
            let path = named_path(dir, name);
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?
        }
        _ => return Err("settings.json: invalid key_layout name".into()),
    })
}

fn checked(name: &str, source: &str, cx: &App) -> Result<Vec<KeyBinding>, String> {
    let (bindings, errors) = settings::parse_keymap(&format!("keymaps/{name}.json"), source, cx);
    if errors.is_empty() {
        Ok(bindings)
    } else {
        Err(errors.join("\n"))
    }
}

fn apply(
    dir: &Path,
    name: &str,
    result: Result<String, String>,
    cx: &mut App,
) -> (Vec<KeyBinding>, Vec<String>) {
    let mut errors: Vec<_> = cx
        .try_global::<Writes>()
        .and_then(|w| w.error.clone())
        .into_iter()
        .map(|e| format!("key_layout: {e}"))
        .collect();
    match result.and_then(|text| checked(name, &text, cx).map(|keys| (text, keys))) {
        Ok((text, keys)) => {
            cx.set_global(Loaded {
                dir: dir.to_owned(),
                name: name.to_owned(),
                source: text,
            });
            (keys, errors)
        }
        Err(error) => {
            errors.push(format!("key_layout: {error}"));
            if let Some(last) = cx.try_global::<Loaded>().filter(|last| last.dir == dir) {
                (
                    checked(&last.name, &last.source, cx).unwrap_or_default(),
                    errors,
                )
            } else {
                cx.set_global(Loaded {
                    dir: dir.to_owned(),
                    name: DEFAULT.into(),
                    source: "[]\n".into(),
                });
                (Vec::new(), errors)
            }
        }
    }
}

fn kept(cx: &App) -> Vec<KeyBinding> {
    cx.try_global::<Loaded>()
        .and_then(|l| checked(&l.name, &l.source, cx).ok())
        .unwrap_or_default()
}

pub fn reload_from(dir: &Path, name: &str, cx: &mut App) -> (Vec<KeyBinding>, Vec<String>) {
    if cx.try_global::<Loaded>().is_none_or(|l| l.dir != dir) {
        cx.set_global(Loaded {
            dir: dir.to_owned(),
            name: DEFAULT.into(),
            source: "[]\n".into(),
        });
        cx.default_global::<Reads>().errors.clear();
    }
    let reads = cx.default_global::<Reads>();
    reads.generation += 1;
    reads.last.take();
    let generation = reads.generation;
    if BUILT_INS.contains(&name) {
        let result = apply(dir, name, source(dir, name), cx);
        cx.default_global::<Reads>().errors = result.1.clone();
        return result;
    }
    // The number of bindings in a user file is not bounded. Reading it
    // belongs off the UI thread; a later reload cancels an older result.
    let dir = dir.to_owned();
    let name = name.to_owned();
    let background = cx.background_executor().clone();
    let task = cx.spawn(async move |cx| {
        let folder = dir.clone();
        let selected = name.clone();
        let result = background
            .spawn(async move { source(&folder, &selected) })
            .await;
        cx.update(|cx| {
            if cx.global::<Reads>().generation != generation || pending(cx) {
                return;
            }
            let (keys, errors) = apply(&dir, &name, result, cx);
            cx.default_global::<Reads>().errors = errors.clone();
            let config = &mut cx.default_global::<settings::ConfigErrors>().0;
            config.retain(|e| !e.starts_with("key_layout:"));
            config.extend(errors);
            settings::bind_key_files(keys, cx);
        })
        .ok();
    });
    cx.default_global::<Reads>().last = Some(task);
    let errors = cx.global::<Reads>().errors.clone();
    (kept(cx), errors)
}

pub fn choose(name: String, cx: &mut App) {
    write(name, None, cx);
}

/// Copies the chosen layer, not the personal overrides that sit on it.
pub fn save_as(name: String, cx: &mut App) {
    let text = cx
        .try_global::<Loaded>()
        .map_or("[]\n", |l| &l.source)
        .to_owned();
    write(name, Some(text), cx);
}

fn select(dir: &Path, name: &str, saved: Option<&str>) -> Result<(), String> {
    let path = dir.join("settings.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}\n".into(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    settings::parse_settings(&text)?;
    let text = if settings::strip_comments(&text).trim().is_empty() {
        "{}\n"
    } else {
        &text
    };
    let text = import::jsonc::set_key(text, "key_layout", &serde_json::json!(name));
    let saved_path = named_path(dir, name);
    if let Some(saved) = saved {
        if !valid_name(name) {
            return Err("keymaps: use letters, numbers, spaces, - or _".into());
        }
        std::fs::create_dir_all(saved_path.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&saved_path)
            .map_err(|e| format!("{}: {e}", saved_path.display()))?;
        if let Err(e) = file.write_all(saved.as_bytes()) {
            std::fs::remove_file(&saved_path).ok();
            return Err(format!("{}: {e}", saved_path.display()));
        }
    }
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let fresh = path.with_extension("json.new");
        std::fs::write(&fresh, text)?;
        std::fs::rename(fresh, &path)
    })();
    if result.is_err() && saved.is_some() {
        std::fs::remove_file(saved_path).ok();
    }
    result.map_err(|e| format!("{}: {e}", path.display()))
}

fn write(name: String, saved: Option<String>, cx: &mut App) {
    let dir = folder(cx);
    let writes = cx.default_global::<Writes>();
    writes.pending += 1;
    let before = writes.last.take();
    let background = cx.background_executor().clone();
    let task = cx.spawn(async move |cx| {
        if let Some(before) = before {
            before.await;
        }
        let folder = dir.clone();
        let chosen = name.clone();
        let text = saved.clone();
        let result = background
            .spawn(async move { text.map_or_else(|| source(&folder, &chosen), Ok) })
            .await;
        let result = result.and_then(|text| {
            cx.update(|cx| checked(&name, &text, cx).map(|_| ()))
                .unwrap_or_else(|e| Err(e.to_string()))
        });
        let result = if result.is_ok() {
            let folder = dir.clone();
            background
                .spawn(async move { select(&folder, &name, saved.as_deref()) })
                .await
        } else {
            result
        };
        cx.update(|cx| {
            let writes = cx.default_global::<Writes>();
            writes.error = result.err();
            writes.pending -= 1;
            if writes.pending == 0 {
                settings::reload_from(&dir, cx);
            }
        })
        .ok();
    });
    cx.default_global::<Writes>().last = Some(task);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_keep_settings_comments_and_never_replace_a_file() {
        let dir = db::testing::dir("key-layout-files");
        std::fs::write(
            dir.join("settings.json"),
            "// mine\n{\"buffer_font_size\":17}\n",
        )
        .unwrap();
        select(&dir, "Мои клавиши", Some("// keys\n[]\n")).unwrap();
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(text.starts_with("// mine\n"));
        let settings = settings::parse_settings(&text).unwrap();
        assert_eq!(settings.buffer_font_size, 17.);
        assert_eq!(settings.key_layout, "Мои клавиши");
        assert!(select(&dir, "Мои клавиши", Some("replaced")).is_err());
        assert_eq!(source(&dir, "Мои клавиши").unwrap(), "// keys\n[]\n");
        for name in ["../outside", "Default", "vs code", "JetBrains", " x", "a/b"] {
            assert!(!valid_name(name), "{name}");
            assert!(select(&dir, name, Some("[]")).is_err());
        }
        std::fs::write(dir.join("keymaps/extra.json.old"), "[]").unwrap();
        assert_eq!(names(&dir), ["Мои клавиши"]);
        std::fs::write(dir.join("settings.json"), "{broken").unwrap();
        assert!(select(&dir, "Unwritten", Some("[]")).is_err());
        assert!(!named_path(&dir, "Unwritten").exists());
    }

    #[gpui::test]
    fn saved_key_layouts_reload_and_keep_the_last_good_bindings(cx: &mut gpui::TestAppContext) {
        use std::time::{Duration, Instant};
        let dir = db::testing::dir("key-layout-watch");
        std::fs::create_dir_all(dir.join("keymaps")).unwrap();
        let file = named_path(&dir, "Mine");
        std::fs::write(&file, "// first\n[]").unwrap();
        std::fs::write(dir.join("settings.json"), "{\"key_layout\":\"Mine\"}").unwrap();
        cx.executor().allow_parking();
        cx.update(|cx| {
            cx.set_global(crate::perf::Perf::new(Instant::now()));
            settings::reload_from(&dir, cx);
            settings::watch_dir(dir.clone(), cx);
        });
        let wait = |cx: &mut gpui::TestAppContext, expected: &str, bad: bool| {
            for _ in 0..300 {
                cx.executor().advance_clock(Duration::from_millis(50));
                cx.run_until_parked();
                if cx.read(|cx| {
                    cx.global::<Loaded>().source == expected
                        && cx.global::<settings::ConfigErrors>().0.is_empty() != bad
                }) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("key file did not reload");
        };
        std::fs::write(&file, "// second\n[]").unwrap();
        wait(cx, "// second\n[]", false);
        std::fs::write(
            &file,
            "[{\"bindings\":{\"ctrl-alt-q\":\"unknown::Action\"}}]",
        )
        .unwrap();
        wait(cx, "// second\n[]", true);
        std::fs::write(&file, "// fixed\n[]").unwrap();
        wait(cx, "// fixed\n[]", false);
        // A read begun for Mine must not select it after a later choice
        // has already restored Default.
        cx.update(|cx| settings::reload_from(&dir, cx));
        std::fs::write(dir.join("settings.json"), "{\"key_layout\":\"Default\"}").unwrap();
        cx.update(|cx| settings::reload_from(&dir, cx));
        wait(cx, "[]\n", false);
        assert_eq!(cx.read(|cx| active(cx).to_owned()), DEFAULT);
    }

    #[gpui::test]
    fn a_missing_set_does_not_reuse_keys_from_another_config(cx: &mut gpui::TestAppContext) {
        use std::time::{Duration, Instant};
        cx.executor().allow_parking();
        let old = db::testing::dir("key-layout-old");
        std::fs::write(old.join("settings.json"), "{\"key_layout\":\"VS Code\"}").unwrap();
        cx.update(|cx| {
            cx.set_global(crate::perf::Perf::new(Instant::now()));
            settings::reload_from(&old, cx);
        });
        assert_eq!(cx.read(|cx| active(cx).to_owned()), VSCODE);
        // An absent file on a fresh config has no cached keys to leak
        // from a different config folder.
        let fresh = db::testing::dir("key-layout-missing");
        std::fs::write(fresh.join("settings.json"), "{\"key_layout\":\"Missing\"}").unwrap();
        cx.update(|cx| settings::reload_from(&fresh, cx));
        for _ in 0..300 {
            cx.executor().advance_clock(Duration::from_millis(50));
            cx.run_until_parked();
            if !cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(cx.read(|cx| active(cx).to_string()), DEFAULT);
        assert!(!cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));
    }
}
