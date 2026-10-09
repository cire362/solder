//! Where the parts of the window are and how large: `layout.json`, next
//! to `settings.json`. The file is the truth. It is read at the start and
//! again whenever it is saved, and what is changed by hand in the window is
//! written back to it, so the two never disagree.
//!
//! A file that cannot be read leaves the last layout that could, and says
//! what is wrong where a mistake in `settings.json` is said. A size outside
//! what a window can show is brought back to the nearest one that fits.
//!
//! There are three docks, left, right and bottom. Each holds panels, with
//! a tab for each: which panels a dock holds and in what order is the
//! file's to say, and a panel can be hidden from all of them.

use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

use crate::settings;

pub const FILE: &str = "layout.json";

pub fn path() -> PathBuf {
    settings::config_dir().join(FILE)
}

/// What a dock can hold. Each is in one dock, or hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Panel {
    Files,
    Search,
    Git,
    Services,
    Database,
    Api,
    Ai,
    Extensions,
    Chat,
    Agent,
    /// The terminals: each has a tab of its own where this panel is.
    Terminal,
    /// The three below have a tab only while they have something to show.
    Debug,
    Response,
    Results,
}

/// One of the three docks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Left,
    Right,
    Bottom,
}

impl Place {
    pub const ALL: [Place; 3] = [Place::Left, Place::Right, Place::Bottom];
}

impl Panel {
    pub const ALL: [Panel; 14] = [
        Panel::Files,
        Panel::Search,
        Panel::Git,
        Panel::Services,
        Panel::Database,
        Panel::Api,
        Panel::Ai,
        Panel::Extensions,
        Panel::Chat,
        Panel::Agent,
        Panel::Terminal,
        Panel::Debug,
        Panel::Response,
        Panel::Results,
    ];

    /// What its tab says.
    pub fn label(self) -> &'static str {
        match self {
            Panel::Files => "Files",
            Panel::Search => "Search",
            Panel::Git => "Git",
            Panel::Services => "Services",
            Panel::Database => "Database",
            Panel::Api => "API",
            Panel::Ai => "AI",
            Panel::Extensions => "Extensions",
            Panel::Chat => "Chat",
            Panel::Agent => "Agent",
            Panel::Terminal => "Terminal",
            Panel::Results => "Results",
            Panel::Response => "Response",
            Panel::Debug => "Debug",
        }
    }

    /// Its name in the file, and in the names tests find its tab by.
    pub fn id(self) -> &'static str {
        match self {
            Panel::Files => "files",
            Panel::Search => "search",
            Panel::Git => "git",
            Panel::Services => "services",
            Panel::Database => "database",
            Panel::Api => "api",
            Panel::Ai => "ai",
            Panel::Extensions => "extensions",
            Panel::Chat => "chat",
            Panel::Agent => "agent",
            Panel::Terminal => "terminal",
            Panel::Results => "results",
            Panel::Response => "response",
            Panel::Debug => "debug",
        }
    }

    /// The dock it is in when the file does not say.
    pub fn home(self) -> Place {
        match self {
            Panel::Chat | Panel::Agent => Place::Right,
            Panel::Terminal | Panel::Results | Panel::Response | Panel::Debug => Place::Bottom,
            _ => Place::Left,
        }
    }
}

/// A dock at the side of the window: its width, and its panels in the
/// order of their tabs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Side {
    pub width: f32,
    pub panels: Vec<Panel>,
}

/// The dock across the bottom of the window: its height, and its panels
/// in the order of their tabs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Low {
    pub height: f32,
    pub panels: Vec<Panel>,
}

/// A bar across the window: its height.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Across {
    pub height: f32,
}

// A part left out of the file has the size it always had; which one that
// is depends on the part, so each is filled in by `Layout`'s own default.
impl Default for Across {
    fn default() -> Self {
        Self { height: 0. }
    }
}

// Each part left out of the file is read as nothing, not as what the
// editor comes with: `fitted` then fills in sizes and puts the panels the
// file does not name where they come, after the ones it does name. Read as
// the standard layout, a dock left out would claim its panels back from
// the dock the file moved them to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// The dock on the left: Files, Search, Git and the rest, as it comes.
    #[serde(default)]
    pub left: Side,
    /// The dock on the right: the chat and the agent, as it comes.
    #[serde(default)]
    pub right: Side,
    /// The dock at the bottom: the terminals, the debugger and results,
    /// as it comes.
    #[serde(default)]
    pub bottom: Low,
    #[serde(default)]
    pub title_bar: Across,
    #[serde(default)]
    pub tab_bar: Across,
    #[serde(default)]
    pub status_bar: Across,
    /// Panels with no tab in any dock. A command still opens one.
    #[serde(default)]
    pub hidden: Vec<Panel>,
}

impl Default for Layout {
    fn default() -> Self {
        let at = |place: Place| -> Vec<Panel> {
            Panel::ALL
                .into_iter()
                .filter(|panel| panel.home() == place)
                .collect()
        };
        Self {
            left: Side {
                width: 390.,
                panels: at(Place::Left),
            },
            right: Side {
                width: 380.,
                panels: at(Place::Right),
            },
            bottom: Low {
                height: 280.,
                panels: at(Place::Bottom),
            },
            title_bar: Across { height: 38. },
            tab_bar: Across { height: 34. },
            status_bar: Across { height: 26. },
            hidden: Vec::new(),
        }
    }
}

impl Global for Layout {}

/// A part whose border can be dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Left,
    Right,
    Bottom,
}

impl Part {
    /// The part's name in the file.
    fn key(self) -> &'static str {
        match self {
            Part::Left => "left",
            Part::Right => "right",
            Part::Bottom => "bottom",
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
    /// The layout in use: the file's, or the one the editor comes with.
    pub fn get(cx: &App) -> &Layout {
        static STANDARD: LazyLock<Layout> = LazyLock::new(Layout::default);
        cx.try_global::<Layout>().unwrap_or(&STANDARD)
    }

    /// The panels of a dock, in the order of their tabs.
    pub fn panels(&self, place: Place) -> &[Panel] {
        match place {
            Place::Left => &self.left.panels,
            Place::Right => &self.right.panels,
            Place::Bottom => &self.bottom.panels,
        }
    }

    /// The dock a panel's tab is in; `None` for a hidden one.
    pub fn place(&self, panel: Panel) -> Option<Place> {
        Place::ALL
            .into_iter()
            .find(|place| self.panels(*place).contains(&panel))
    }

    /// The size of a part that can be dragged: a width or a height.
    pub fn size(&self, part: Part) -> f32 {
        match part {
            Part::Left => self.left.width,
            Part::Right => self.right.width,
            Part::Bottom => self.bottom.height,
        }
    }

    /// The layout with `part` at `size`, or the nearest size that fits.
    pub fn with(mut self, part: Part, size: f32) -> Self {
        // Nothing at all would mean the size it came with; a border
        // dragged shut means the smallest.
        let size = size.max(1.);
        match part {
            Part::Left => self.left.width = size,
            Part::Right => self.right.width = size,
            Part::Bottom => self.bottom.height = size,
        }
        self.fitted()
    }

    /// The same layout with every size one a window can show (a part left
    /// out of the file, or given as nothing, gets the size it came with)
    /// and every panel in one place: a panel named twice stays where it
    /// was named first, a hidden one is in no dock, and one the file does
    /// not name at all goes to the dock it comes in, after the others.
    fn fitted(mut self) -> Self {
        let mut placed: Vec<Panel> = Vec::new();
        let mut hidden: Vec<Panel> = Vec::new();
        for panel in std::mem::take(&mut self.hidden) {
            if !hidden.contains(&panel) {
                hidden.push(panel);
            }
        }
        for panels in [
            &mut self.left.panels,
            &mut self.right.panels,
            &mut self.bottom.panels,
        ] {
            panels.retain(|panel| {
                let keep = !hidden.contains(panel) && !placed.contains(panel);
                if keep {
                    placed.push(*panel);
                }
                keep
            });
        }
        for panel in Panel::ALL {
            if !placed.contains(&panel) && !hidden.contains(&panel) {
                match panel.home() {
                    Place::Left => self.left.panels.push(panel),
                    Place::Right => self.right.panels.push(panel),
                    Place::Bottom => self.bottom.panels.push(panel),
                }
            }
        }
        self.hidden = hidden;

        let standard = Layout::default();
        let fit = |size: &mut f32, standard: f32, (least, most): (f32, f32)| {
            *size = if size.is_finite() && *size > 0. {
                size.clamp(least, most).round()
            } else {
                standard
            };
        };
        fit(&mut self.left.width, standard.left.width, SIDE);
        fit(&mut self.right.width, standard.right.width, SIDE);
        fit(&mut self.bottom.height, standard.bottom.height, DOCK);
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
        Part::Left => serde_json::to_value(&layout.left),
        Part::Right => serde_json::to_value(&layout.right),
        Part::Bottom => serde_json::to_value(&layout.bottom),
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
            parse(r#"{ "left": { "width": 300 }, "bottom": { "height": 420.4 } }"#).unwrap();
        assert_eq!(layout.left.width, 300.);
        assert_eq!(layout.left.panels, Layout::default().left.panels);
        assert_eq!(layout.bottom.height, 420.);
        assert_eq!(layout.bottom.panels, Layout::default().bottom.panels);
        assert_eq!(layout.right, Layout::default().right);
        assert_eq!(layout.status_bar, Layout::default().status_bar);
        // A size no window can show is brought to the nearest that fits,
        // and nothing at all is the size it came with.
        let odd = parse(
            r#"{ "left": { "width": 20 }, "right": { "width": 5000 }, "tab_bar": { "height": 0 }, "status_bar": {} }"#,
        )
        .unwrap();
        assert_eq!(odd.left.width, 200.);
        assert_eq!(odd.right.width, 900.);
        assert_eq!(odd.tab_bar, Layout::default().tab_bar);
        assert_eq!(odd.status_bar, Layout::default().status_bar);
        // A mistake names the file and where it is.
        let error = parse(r#"{ "left": { "width": "wide" } }"#).unwrap_err();
        assert!(error.starts_with("layout.json: "), "{error}");
        let error = parse(r#"{ "lefts": {} }"#).unwrap_err();
        assert!(error.contains("lefts"), "{error}");
        let error = parse(r#"{ "left": { "panels": ["nothing"] } }"#).unwrap_err();
        assert!(error.contains("nothing"), "{error}");
        // What the editor writes is what it reads.
        assert_eq!(parse(&file(&layout)).unwrap(), layout);
    }

    #[test]
    fn every_panel_is_in_one_dock_or_hidden() {
        use Panel::*;
        // As it comes: eight on the left, the chat and the agent on the
        // right, the terminals, the debugger and the two kinds of answers
        // at the bottom, none hidden.
        let standard = Layout::default();
        assert_eq!(
            standard.left.panels,
            [Files, Search, Git, Services, Database, Api, Ai, Extensions]
        );
        assert_eq!(standard.right.panels, [Chat, Agent]);
        assert_eq!(standard.bottom.panels, [Terminal, Debug, Response, Results]);
        assert_eq!(standard.place(Terminal), Some(Place::Bottom));
        assert_eq!(standard.place(Git), Some(Place::Left));
        assert_eq!(standard.place(Agent), Some(Place::Right));

        // The file moves panels, orders them and hides one. A panel named
        // twice stays where it was named first; a hidden one is in no
        // dock even if a dock names it; the ones the file leaves out go
        // where they come, after the ones it names.
        let layout = parse(
            r#"{
              "left": { "panels": ["chat", "git", "files", "chat"] },
              "right": { "panels": ["search", "git", "api", "terminal"] },
              "bottom": { "panels": ["agent", "terminal"] },
              "hidden": ["api", "ai", "api"]
            }"#,
        )
        .unwrap();
        assert_eq!(
            layout.left.panels,
            [Chat, Git, Files, Services, Database, Extensions]
        );
        assert_eq!(layout.right.panels, [Search, Terminal]);
        assert_eq!(layout.bottom.panels, [Agent, Debug, Response, Results]);
        assert_eq!(layout.hidden, [Api, Ai]);
        assert_eq!(layout.place(Terminal), Some(Place::Right));
        assert_eq!(layout.place(Agent), Some(Place::Bottom));
        assert_eq!(layout.place(Chat), Some(Place::Left));
        assert_eq!(layout.place(Search), Some(Place::Right));
        assert_eq!(layout.place(Api), None);
        for panel in Panel::ALL {
            let places = [
                &layout.left.panels,
                &layout.right.panels,
                &layout.bottom.panels,
                &layout.hidden,
            ]
            .iter()
            .filter(|list| list.contains(&panel))
            .count();
            assert_eq!(places, 1, "{panel:?}");
        }
        // Written out and read again, it is the same.
        assert_eq!(parse(&file(&layout)).unwrap(), layout);
        // A dock can be emptied: everything it had is elsewhere.
        let empty = parse(
            r#"{ "right": { "panels": ["files", "search", "git", "services", "database", "api", "ai", "extensions", "chat", "agent"] } }"#,
        )
        .unwrap();
        assert!(empty.left.panels.is_empty());
        assert_eq!(empty.right.panels.len(), 10);
        assert_eq!(empty.bottom.panels.len(), 4);
    }
}
