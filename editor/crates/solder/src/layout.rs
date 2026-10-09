//! Where the parts of the window are and how large: `layout.json`, next
//! to `settings.json`. The file is the truth. It is read at the start and
//! again whenever it is saved, and what is changed by hand in the window is
//! written back to it, so the two never disagree.
//!
//! A file that cannot be read leaves the last layout that could, and says
//! what is wrong where a mistake in `settings.json` is said. A size outside
//! what a window can show is brought back to the nearest one that fits.

use std::path::{Path, PathBuf};

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

use crate::settings;

pub const FILE: &str = "layout.json";

pub fn path() -> PathBuf {
    settings::config_dir().join(FILE)
}

/// A panel at the side of the window: its width.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Side {
    pub width: f32,
}

/// A bar or a dock across the window: its height.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Across {
    pub height: f32,
}

// A part left out of the file has the size it always had; which one that
// is depends on the part, so each is filled in by `Layout`'s own default.
impl Default for Side {
    fn default() -> Self {
        Self { width: 0. }
    }
}

impl Default for Across {
    fn default() -> Self {
        Self { height: 0. }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Layout {
    /// Files, Search, Git and the other tabs on the left.
    pub sidebar: Side,
    /// The chat and the agent on the right.
    pub chat: Side,
    /// The terminal, results and the debugger at the bottom.
    pub dock: Across,
    pub title_bar: Across,
    pub tab_bar: Across,
    pub status_bar: Across,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar: Side { width: 390. },
            chat: Side { width: 380. },
            dock: Across { height: 280. },
            title_bar: Across { height: 38. },
            tab_bar: Across { height: 34. },
            status_bar: Across { height: 26. },
        }
    }
}

impl Global for Layout {}

/// A part whose border can be dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Sidebar,
    Chat,
    Dock,
}

impl Part {
    /// The part's name in the file.
    fn key(self) -> &'static str {
        match self {
            Part::Sidebar => "sidebar",
            Part::Chat => "chat",
            Part::Dock => "dock",
        }
    }
}

/// The folder the layout was last read from, which is where a change made
/// by hand in the window is written. Nothing is written before a file was
/// looked for there: a test that never loads one writes none.
struct Home(PathBuf);

impl Global for Home {}

/// The sizes each part may have. Below the first a part cannot hold what
/// is in it; above the second it leaves no room for the rest.
const SIDE: (f32, f32) = (200., 900.);
const DOCK: (f32, f32) = (100., 1200.);
const BAR: (f32, f32) = (22., 64.);

impl Layout {
    pub fn get(cx: &App) -> Layout {
        cx.try_global::<Layout>().copied().unwrap_or_default()
    }

    /// The size of a part that can be dragged: a width or a height.
    pub fn size(&self, part: Part) -> f32 {
        match part {
            Part::Sidebar => self.sidebar.width,
            Part::Chat => self.chat.width,
            Part::Dock => self.dock.height,
        }
    }

    /// The layout with `part` at `size`, or the nearest size that fits.
    pub fn with(mut self, part: Part, size: f32) -> Self {
        // Nothing at all would mean the size it came with; a border
        // dragged shut means the smallest.
        let size = size.max(1.);
        match part {
            Part::Sidebar => self.sidebar.width = size,
            Part::Chat => self.chat.width = size,
            Part::Dock => self.dock.height = size,
        }
        self.fitted()
    }

    /// The same layout with every size one a window can show: a part left
    /// out of the file, or given as nothing, gets the size it came with.
    fn fitted(mut self) -> Self {
        let standard = Layout::default();
        let fit = |size: &mut f32, standard: f32, (least, most): (f32, f32)| {
            *size = if size.is_finite() && *size > 0. {
                size.clamp(least, most).round()
            } else {
                standard
            };
        };
        fit(&mut self.sidebar.width, standard.sidebar.width, SIDE);
        fit(&mut self.chat.width, standard.chat.width, SIDE);
        fit(&mut self.dock.height, standard.dock.height, DOCK);
        fit(&mut self.title_bar.height, standard.title_bar.height, BAR);
        fit(&mut self.tab_bar.height, standard.tab_bar.height, BAR);
        fit(&mut self.status_bar.height, standard.status_bar.height, BAR);
        self
    }
}

/// Reads a layout file's text. An empty file is the layout the editor
/// comes with; comments are allowed, as in the other config files.
pub fn parse(source: &str) -> Result<Layout, String> {
    let stripped = settings::strip_comments(source);
    if stripped.trim().is_empty() {
        return Ok(Layout::default());
    }
    serde_json::from_str::<Layout>(&stripped)
        .map(Layout::fitted)
        .map_err(|e| format!("{FILE}: {e}"))
}

/// The file a user starts from: every part with the size it has now.
pub fn file(layout: &Layout) -> String {
    let json = serde_json::to_string_pretty(layout).unwrap_or_default();
    format!(
        "// Where the parts of the window are and how large. Applied as soon as you save.\n{json}\n"
    )
}

/// Reads the file in `dir` and puts its layout in use. On a mistake the
/// layout in use stays and the mistake is returned.
pub fn reload_from(dir: &Path, cx: &mut App) -> Option<String> {
    cx.set_global(Home(dir.to_path_buf()));
    let source = std::fs::read_to_string(dir.join(FILE)).unwrap_or_default();
    match parse(&source) {
        Ok(layout) => {
            if cx.try_global::<Layout>() != Some(&layout) {
                cx.set_global(layout);
            }
            None
        }
        Err(error) => {
            if cx.try_global::<Layout>().is_none() {
                cx.set_global(Layout::default());
            }
            Some(error)
        }
    }
}

/// Writes the size `part` has now into the file, leaving the rest of the
/// file as the user wrote it. A file with a mistake in it is left alone:
/// it is the user's to put right.
pub fn keep(part: Part, cx: &mut App) {
    let Some(dir) = cx.try_global::<Home>().map(|home| home.0.clone()) else {
        return;
    };
    let layout = Layout::get(cx);
    let value = match part {
        Part::Sidebar => serde_json::to_value(layout.sidebar),
        Part::Chat => serde_json::to_value(layout.chat),
        Part::Dock => serde_json::to_value(layout.dock),
    };
    let Ok(value) = value else { return };
    cx.background_executor()
        .spawn(async move {
            let path = dir.join(FILE);
            let text = std::fs::read_to_string(&path)
                .ok()
                .filter(|text| !settings::strip_comments(text).trim().is_empty())
                .unwrap_or_else(|| "{\n}\n".into());
            if parse(&text).is_err() {
                return;
            }
            let text = import::jsonc::set_key(&text, part.key(), &value);
            // Whole or not at all: the file is read as soon as it changes.
            let fresh = dir.join(format!("{FILE}.new"));
            if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&fresh, text).is_ok() {
                let _ = std::fs::rename(&fresh, &path);
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_file_gives_sizes_and_the_rest_stay() {
        // Nothing in the file, or no file: the layout the editor comes with.
        assert_eq!(parse("").unwrap(), Layout::default());
        assert_eq!(parse("// only a comment\n{}").unwrap(), Layout::default());
        // One part named: the others are as they were.
        let layout =
            parse(r#"{ "sidebar": { "width": 300 }, "dock": { "height": 420.4 } }"#).unwrap();
        assert_eq!(layout.sidebar.width, 300.);
        assert_eq!(layout.dock.height, 420.);
        assert_eq!(layout.chat, Layout::default().chat);
        assert_eq!(layout.status_bar, Layout::default().status_bar);
        // A size no window can show is brought to the nearest that fits,
        // and nothing at all is the size it came with.
        let odd = parse(
            r#"{ "sidebar": { "width": 20 }, "chat": { "width": 5000 }, "tab_bar": { "height": 0 }, "status_bar": {} }"#,
        )
        .unwrap();
        assert_eq!(odd.sidebar.width, 200.);
        assert_eq!(odd.chat.width, 900.);
        assert_eq!(odd.tab_bar, Layout::default().tab_bar);
        assert_eq!(odd.status_bar, Layout::default().status_bar);
        // A mistake names the file and where it is.
        let error = parse(r#"{ "sidebar": { "width": "wide" } }"#).unwrap_err();
        assert!(error.starts_with("layout.json: "), "{error}");
        let error = parse(r#"{ "sidebars": {} }"#).unwrap_err();
        assert!(error.contains("sidebars"), "{error}");
        // What the editor writes is what it reads.
        assert_eq!(parse(&file(&layout)).unwrap(), layout);
    }
}
