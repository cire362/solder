//! Where the parts of the window are and how large: `layout.json`, next
//! to `settings.json`. The file is the truth. It is read at the start and
//! again whenever it is saved, and what is changed by hand in the window is
//! written back to it, so the two never disagree.
//! A named layout uses `layouts/<name>.json` instead. `layouts.json`
//! remembers the choice for the app and for each open project.
//!
//! A file that cannot be read leaves the last layout that could, and says
//! what is wrong where a mistake in `settings.json` is said. A size outside
//! what a window can show is brought back to the nearest one that fits.
//!
//! There are three docks, left, right and bottom. Each holds panels, with
//! a tab for each: which panels a dock holds and in what order is the
//! file's to say, and a panel can be hidden from all of them. Which panel
//! each dock has open is there too, so the window starts as it was left.
//!
//! The title bar and the status bar hold items, from the left end and
//! from the right one. An item is in one place, or in none.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::LazyLock,
};

use gpui::{App, Global, Task, Window};
use serde::{Deserialize, Serialize};

use crate::settings;

pub const FILE: &str = "layout.json";
const CHOICES: &str = "layouts.json";
pub const DEFAULT_NAME: &str = "Default";

pub fn path(cx: &App) -> PathBuf {
    let dir = folder(cx);
    layout_path(&dir, active(cx).as_deref())
}

pub fn folder(cx: &App) -> PathBuf {
    cx.try_global::<Home>()
        .map(|home| home.dir.clone())
        .unwrap_or_else(settings::config_dir)
}

pub fn active(cx: &App) -> Option<String> {
    cx.try_global::<Home>()
        .and_then(|home| home.choices.active.clone())
}

fn layout_path(dir: &Path, name: Option<&str>) -> PathBuf {
    match name {
        Some(name) => dir.join("layouts").join(format!("{name}.json")),
        None => dir.join(FILE),
    }
}

/// A name is one file's stem, never a path. Unicode names are welcome.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && name.trim() == name
        && !name.eq_ignore_ascii_case(DEFAULT_NAME)
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || " -_".contains(c))
}

pub fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir.join("layouts"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension()? == "json").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .filter(|name| valid_name(name))
        .collect();
    names.sort();
    names
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
    ExtensionViews,
    Tests,
    /// The outline of the file in front.
    Structure,
    /// What the language servers report of the project's files.
    Problems,
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

    /// Its name in the file.
    pub fn id(self) -> &'static str {
        match self {
            Place::Left => "left",
            Place::Right => "right",
            Place::Bottom => "bottom",
        }
    }
}

impl Panel {
    pub const ALL: [Panel; 18] = [
        Panel::Files,
        Panel::Search,
        Panel::Git,
        Panel::Services,
        Panel::Database,
        Panel::Api,
        Panel::Ai,
        Panel::Extensions,
        Panel::ExtensionViews,
        Panel::Tests,
        Panel::Structure,
        Panel::Chat,
        Panel::Agent,
        Panel::Terminal,
        Panel::Problems,
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
            Panel::ExtensionViews => "Views",
            Panel::Tests => "Tests",
            Panel::Structure => "Structure",
            Panel::Problems => "Problems",
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
            Panel::ExtensionViews => "extension_views",
            Panel::Tests => "tests",
            Panel::Structure => "structure",
            Panel::Problems => "problems",
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
            Panel::Terminal | Panel::Results | Panel::Response | Panel::Debug | Panel::Problems => {
                Place::Bottom
            }
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

/// What a bar can hold. Each says one thing about the window, and is not
/// drawn while it has nothing to say.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Item {
    /// The project's name.
    Project,
    /// What is in front: the file, or the view that took its place.
    File,
    /// Where the cursor is: the file's path, and the symbols it is in.
    Breadcrumbs,
    /// The branch the repository is on. Opens the list of branches.
    Branch,
    /// The line and the column of the cursor, and how many cursors.
    Position,
    Indent,
    /// What the file is written in and what its lines end with, where
    /// either is not the usual (UTF-8, LF). Opens the list of encodings.
    Encoding,
    Language,
    /// How many errors and warnings the file has.
    Problems,
    /// What is going on: a language server starting, files being read.
    Activity,
    /// The database a query file runs against. Opens the list of them.
    Connection,
    /// What the code of extensions shows: its status bar items, work in
    /// progress, and the last message.
    Extensions,
    /// What plugins show. Opens the Plugins window.
    Plugins,
    /// The numbers of `show_performance_hud`.
    Performance,
    /// A button for a command, written `{ "button": "name" }`. What it
    /// runs and how it looks is under that name in `items`.
    #[serde(untagged)]
    Button {
        button: String,
    },
}

// Read by hand: left to serde, a name that is no item was said to match
// "no variant", without the name that would tell the user which line.
impl<'de> Deserialize<'de> for Item {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Name(String),
            Button { button: String },
        }
        let unknown = "an item's name or { \"button\": \"name\" }";
        match Written::deserialize(deserializer).map_err(|_| serde::de::Error::custom(unknown))? {
            Written::Button { button } => Ok(Item::Button { button }),
            Written::Name(name) => Item::ALL
                .into_iter()
                .find(|item| item.id() == name)
                .ok_or_else(|| serde::de::Error::custom(format!("no item \u{201c}{name}\u{201d}"))),
        }
    }
}

impl Item {
    pub const ALL: [Item; 14] = [
        Item::Project,
        Item::File,
        Item::Breadcrumbs,
        Item::Branch,
        Item::Position,
        Item::Indent,
        Item::Encoding,
        Item::Language,
        Item::Problems,
        Item::Activity,
        Item::Connection,
        Item::Extensions,
        Item::Plugins,
        Item::Performance,
    ];

    pub fn label(&self) -> &str {
        match self {
            Item::Project => "Project",
            Item::File => "File",
            Item::Breadcrumbs => "Breadcrumbs",
            Item::Branch => "Branch",
            Item::Position => "Position",
            Item::Indent => "Indent",
            Item::Encoding => "Encoding",
            Item::Language => "Language",
            Item::Problems => "Problems",
            Item::Activity => "Activity",
            Item::Connection => "Connection",
            Item::Extensions => "Extensions",
            Item::Plugins => "Plugins",
            Item::Performance => "Performance",
            Item::Button { button } => button,
        }
    }

    /// Its name in the file, and in the names tests find it by.
    pub fn id(&self) -> &str {
        match self {
            Item::Project => "project",
            Item::File => "file",
            Item::Breadcrumbs => "breadcrumbs",
            Item::Branch => "branch",
            Item::Position => "position",
            Item::Indent => "indent",
            Item::Encoding => "encoding",
            Item::Language => "language",
            Item::Problems => "problems",
            Item::Activity => "activity",
            Item::Connection => "connection",
            Item::Extensions => "extensions",
            Item::Plugins => "plugins",
            Item::Performance => "performance",
            Item::Button { button } => button,
        }
    }
}

/// How an item of a bar is drawn: as a word, an icon or both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Display {
    Text,
    Icon,
    #[default]
    Both,
}

/// The same action notation as a key binding; plugins keep their owner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Command {
    Action(String),
    Args((String, serde_json::Value)),
    Plugin { plugin: String, command: String },
}

/// What `items` says of one item: how it is drawn, and for a button the
/// command it runs. On an item of the editor's own, a command or a label
/// takes the place of what the item did and said.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ItemStyle {
    pub display: Display,
    pub icon: Option<String>,
    pub label: Option<String>,
    pub command: Option<Command>,
}

/// One of the four lists an item can be moved into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarEnd {
    TitleLeft,
    TitleRight,
    StatusLeft,
    StatusRight,
}

impl BarEnd {
    pub const ALL: [BarEnd; 4] = [
        BarEnd::TitleLeft,
        BarEnd::TitleRight,
        BarEnd::StatusLeft,
        BarEnd::StatusRight,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::TitleLeft => "title-left",
            Self::TitleRight => "title-right",
            Self::StatusLeft => "status-left",
            Self::StatusRight => "status-right",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TitleLeft => "title bar, left",
            Self::TitleRight => "title bar, right",
            Self::StatusLeft => "status bar, left",
            Self::StatusRight => "status bar, right",
        }
    }

    pub fn is_right(self) -> bool {
        matches!(self, Self::TitleRight | Self::StatusRight)
    }
}

/// The items of the bars as they come. `file` and `branch` are in none.
const TITLE_LEFT: &[Item] = &[Item::Project, Item::Breadcrumbs];
const TITLE_RIGHT: &[Item] = &[];
const STATUS_LEFT: &[Item] = &[
    Item::Position,
    Item::Indent,
    Item::Encoding,
    Item::Language,
    Item::Problems,
    Item::Activity,
    Item::Connection,
];
const STATUS_RIGHT: &[Item] = &[Item::Extensions, Item::Plugins, Item::Performance];

/// A bar across the window: its height, and its items from each end. An
/// end the file leaves out has the items it comes with, less those the
/// file put elsewhere; `fitted` fills it in, so in a layout in use both
/// are there.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bar {
    pub height: f32,
    pub left: Option<Vec<Item>>,
    pub right: Option<Vec<Item>>,
}

impl Bar {
    pub fn left(&self) -> &[Item] {
        self.left.as_deref().unwrap_or_default()
    }

    pub fn right(&self) -> &[Item] {
        self.right.as_deref().unwrap_or_default()
    }
}

/// Where the tabs of the open files are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabsAt {
    /// Above the file, as they come.
    #[default]
    Top,
    Bottom,
    /// Nowhere: files are changed by the keys and the file finder.
    None,
}

/// The bar of a pane's tabs: its height and where it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TabBar {
    pub height: f32,
    pub place: TabsAt,
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
    pub title_bar: Bar,
    #[serde(default)]
    pub tab_bar: TabBar,
    #[serde(default)]
    pub status_bar: Bar,
    /// Panels with no tab in any dock. A command still opens one.
    #[serde(default)]
    pub hidden: Vec<Panel>,
    /// The panel each dock has open, one a dock at most; a dock with none
    /// of its panels here is closed. Until the file says, the left dock is
    /// open on its first panel and the others are closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Vec<Panel>>,
    /// Appearance and command overrides, including named buttons.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub items: BTreeMap<String, ItemStyle>,
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
            title_bar: Bar {
                height: 38.,
                left: Some(TITLE_LEFT.to_vec()),
                right: Some(TITLE_RIGHT.to_vec()),
            },
            tab_bar: TabBar {
                height: 34.,
                place: TabsAt::Top,
            },
            status_bar: Bar {
                height: 26.,
                left: Some(STATUS_LEFT.to_vec()),
                right: Some(STATUS_RIGHT.to_vec()),
            },
            hidden: Vec::new(),
            open: None,
            items: BTreeMap::new(),
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
struct Home {
    dir: PathBuf,
    choices: Choices,
}

impl Global for Home {}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Choices {
    active: Option<String>,
    projects: BTreeMap<PathBuf, Option<String>>,
}

impl Choices {
    fn read(dir: &Path) -> Result<Self, String> {
        let text = read(&dir.join(CHOICES), true)?;
        if settings::strip_comments(&text).trim().is_empty() {
            return Ok(Self::default());
        }
        let choices: Self = serde_json::from_str(&settings::strip_comments(&text))
            .map_err(|error| format!("{CHOICES}: {error}"))?;
        if choices
            .active
            .iter()
            .chain(choices.projects.values().flatten())
            .any(|name| !valid_name(name))
        {
            return Err(format!("{CHOICES}: invalid layout name"));
        }
        Ok(choices)
    }

    fn select(dir: &Path, name: Option<String>, projects: Vec<PathBuf>) -> Result<Self, String> {
        let mut choices = Self::read(dir)?;
        choices.active = name.clone();
        for project in projects {
            choices.projects.insert(project, name.clone());
        }
        let text = serde_json::to_string_pretty(&choices).map_err(|e| e.to_string())?;
        replace(&dir.join(CHOICES), &format!("{text}\n"))?;
        Ok(choices)
    }
}

/// Window ids let a global switch remember exactly the projects still
/// open, without borrowing any workspace while one handles the command.
#[derive(Default)]
struct Projects(HashMap<gpui::WindowId, PathBuf>);
impl Global for Projects {}

#[derive(Default)]
struct Failure(Option<String>);
impl Global for Failure {}

/// Changing layouts restores their open docks; an edit to the same file
/// can still move an open panel with its tab, as it did before names.
#[derive(Default)]
struct Selection(u64);
impl Global for Selection {}

pub fn selection(cx: &App) -> u64 {
    cx.try_global::<Selection>()
        .map_or(0, |selection| selection.0)
}

fn selected(cx: &mut App) {
    cx.default_global::<Selection>().0 += 1;
}

pub fn for_project(root: &Path, window: &Window, cx: &mut App) {
    cx.default_global::<Projects>()
        .0
        .insert(window.window_handle().window_id(), root.to_path_buf());
    let choice = cx
        .try_global::<Home>()
        .and_then(|home| home.choices.projects.get(root).cloned());
    if let Some(name) = choice
        && name != active(cx)
    {
        choose(name, cx);
    }
}

pub fn choose(name: Option<String>, cx: &mut App) {
    write(
        Change::Choose {
            name,
            projects: open_projects(cx),
        },
        cx,
    );
}

pub fn save_as(name: String, cx: &mut App) {
    write(
        Change::Save {
            name,
            layout: Box::new(Layout::get(cx).clone()),
            projects: open_projects(cx),
        },
        cx,
    );
}

fn open_projects(cx: &mut App) -> Vec<PathBuf> {
    let windows = cx.windows();
    let projects = cx.default_global::<Projects>();
    projects
        .0
        .retain(|id, _| windows.iter().any(|window| window.window_id() == *id));
    projects.0.values().cloned().collect()
}

/// Changes of ours that have not reached the file yet. They go one after
/// another: each reads the file and puts one key in it, and two at once
/// would lose one of the keys.
#[derive(Default)]
struct Writes {
    last: Option<Task<()>>,
    pending: usize,
}

impl Global for Writes {}

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

    /// The dock a panel shows in: the one its tab is in, or for a hidden
    /// panel, which a command can still open, the one it comes in.
    pub fn dock_of(&self, panel: Panel) -> Place {
        self.place(panel).unwrap_or(panel.home())
    }

    /// The panel a dock has open: the one the file names, or as it comes.
    pub fn open_in(&self, place: Place) -> Option<Panel> {
        match &self.open {
            Some(open) => open
                .iter()
                .copied()
                .find(|panel| self.dock_of(*panel) == place),
            None => match place {
                Place::Left => self.left.panels.first().copied(),
                Place::Right | Place::Bottom => None,
            },
        }
    }

    /// The four ends of the two bars, each with the items it comes with.
    fn ends(&mut self) -> [(&mut Option<Vec<Item>>, &'static [Item]); 4] {
        [
            (&mut self.title_bar.left, TITLE_LEFT),
            (&mut self.title_bar.right, TITLE_RIGHT),
            (&mut self.status_bar.left, STATUS_LEFT),
            (&mut self.status_bar.right, STATUS_RIGHT),
        ]
    }

    pub fn items(&self, end: BarEnd) -> &[Item] {
        match end {
            BarEnd::TitleLeft => self.title_bar.left(),
            BarEnd::TitleRight => self.title_bar.right(),
            BarEnd::StatusLeft => self.status_bar.left(),
            BarEnd::StatusRight => self.status_bar.right(),
        }
    }

    pub fn item_place(&self, item: impl std::borrow::Borrow<Item>) -> Option<BarEnd> {
        BarEnd::ALL
            .into_iter()
            .find(|end| self.items(*end).contains(item.borrow()))
    }

    pub fn style(&self, item: &Item) -> &ItemStyle {
        static STANDARD: LazyLock<ItemStyle> = LazyLock::new(ItemStyle::default);
        self.items.get(item.id()).unwrap_or(&STANDARD)
    }

    pub fn styled(mut self, item: &Item, display: Display) -> Self {
        self.items.entry(item.id().to_owned()).or_default().display = display;
        self
    }

    /// Every item there is: the editor's own, then the buttons `items`
    /// names.
    pub fn named_items(&self) -> Vec<Item> {
        let buttons = self
            .items
            .keys()
            .filter(|name| !Item::ALL.iter().any(|item| item.id() == *name))
            .map(|name| Item::Button {
                button: name.clone(),
            });
        Item::ALL.into_iter().chain(buttons).collect()
    }

    /// The layout without a button: off the bars, and out of `items`, so
    /// that it is not offered among the hidden ones either.
    pub fn without_button(self, button: &str) -> Self {
        let mut layout = self.hiding_item(Item::Button {
            button: button.to_owned(),
        });
        layout.items.remove(button);
        layout
    }

    pub fn add_button(mut self, label: String, command: Command, end: BarEnd) -> Self {
        let button = (1..)
            .map(|n| format!("button-{n}"))
            .find(|name| !self.items.contains_key(name))
            .unwrap();
        self.items.insert(
            button.clone(),
            ItemStyle {
                label: Some(label),
                command: Some(command),
                icon: Some("gear".into()),
                ..Default::default()
            },
        );
        self.moved_item(Item::Button { button }, end, None)
    }

    fn check_items(&self) -> Result<(), String> {
        for (name, style) in &self.items {
            let known = Item::ALL.iter().any(|item| item.id() == name);
            if !known
                && (name.is_empty()
                    || name.len() > 80
                    || !name
                        .chars()
                        .all(|c| c.is_alphanumeric() || "-_".contains(c))
                    || style.command.is_none())
            {
                return Err(format!(
                    "{FILE}: item {name}: a button needs a name and command"
                ));
            }
            if style
                .icon
                .as_deref()
                .is_some_and(|icon| crate::icons::named(icon).is_none())
            {
                return Err(format!("{FILE}: item {name}: unknown icon"));
            }
            if style
                .label
                .as_ref()
                .is_some_and(|label| label.trim().is_empty() || label.len() > 256)
            {
                return Err(format!("{FILE}: item {name}: use a short label"));
            }
        }
        for end in BarEnd::ALL {
            for item in self.items(end) {
                if let Item::Button { button } = item
                    && (Item::ALL.iter().any(|item| item.id() == button)
                        || !self.items.contains_key(button))
                {
                    return Err(format!("{FILE}: unknown button {button}"));
                }
            }
        }
        Ok(())
    }

    fn check_commands(&self, cx: &App) -> Result<(), String> {
        for (name, style) in &self.items {
            let action = match &style.command {
                Some(Command::Action(action)) => Some((action, None)),
                Some(Command::Args((action, args))) => Some((action, Some(args.clone()))),
                Some(Command::Plugin { plugin, command })
                    if plugin.is_empty() || command.is_empty() =>
                {
                    return Err(format!(
                        "{FILE}: item {name}: a plugin command needs its owner and id"
                    ));
                }
                _ => None,
            };
            if let Some((action, args)) = action {
                cx.build_action(action, args)
                    .map_err(|error| format!("{FILE}: item {name}: {error}"))?;
            }
        }
        Ok(())
    }

    fn items_mut(&mut self, end: BarEnd) -> &mut Vec<Item> {
        let items = match end {
            BarEnd::TitleLeft => &mut self.title_bar.left,
            BarEnd::TitleRight => &mut self.title_bar.right,
            BarEnd::StatusLeft => &mut self.status_bar.left,
            BarEnd::StatusRight => &mut self.status_bar.right,
        };
        items.get_or_insert_default()
    }

    /// Materialize defaults before removing an item, so it cannot come
    /// back through a list the file had left implicit.
    pub fn hiding_item(self, item: Item) -> Self {
        let mut layout = self.fitted();
        for end in BarEnd::ALL {
            layout.items_mut(end).retain(|other| other != &item);
        }
        layout
    }

    pub fn moved_item(self, item: Item, to: BarEnd, before: Option<Item>) -> Self {
        if before.as_ref() == Some(&item) {
            return self;
        }
        let mut layout = self.hiding_item(item.clone());
        let items = layout.items_mut(to);
        let at = before
            .and_then(|before| items.iter().position(|other| *other == before))
            .unwrap_or(items.len());
        items.insert(at, item);
        layout
    }

    fn panels_mut(&mut self, place: Place) -> &mut Vec<Panel> {
        match place {
            Place::Left => &mut self.left.panels,
            Place::Right => &mut self.right.panels,
            Place::Bottom => &mut self.bottom.panels,
        }
    }

    /// The layout with `panel` in the dock `to`, before `before` or after
    /// the dock's other panels. It leaves the dock it was in, or stops
    /// being hidden.
    pub fn moved(mut self, panel: Panel, to: Place, before: Option<Panel>) -> Self {
        if before == Some(panel) {
            return self;
        }
        for place in Place::ALL {
            self.panels_mut(place).retain(|other| *other != panel);
        }
        self.hidden.retain(|other| *other != panel);
        let panels = self.panels_mut(to);
        let at = before
            .and_then(|before| panels.iter().position(|other| *other == before))
            .unwrap_or(panels.len());
        panels.insert(at, panel);
        self.fitted()
    }

    /// The layout with `panel` in no dock.
    pub fn hiding(mut self, panel: Panel) -> Self {
        if !self.hidden.contains(&panel) {
            self.hidden.push(panel);
        }
        self.fitted()
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
    /// not name at all goes to the dock it comes in, after the others. Of
    /// the panels named as open, the first of each dock is, and they are
    /// kept in the order of the docks, so that the same window is always
    /// written the same way. An item of a bar named twice stays where it
    /// was named first, and an end of a bar the file leaves out has the
    /// items it comes with that the file names nowhere.
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
        if let Some(open) = self.open.take() {
            let mut docks = [None; 3];
            for panel in open {
                docks[self.dock_of(panel) as usize].get_or_insert(panel);
            }
            self.open = Some(docks.into_iter().flatten().collect());
        }

        let mut named: Vec<Item> = Vec::new();
        for (end, _) in self.ends() {
            if let Some(items) = end {
                items.retain(|item| {
                    let keep = !named.contains(item);
                    if keep {
                        named.push(item.clone());
                    }
                    keep
                });
            }
        }
        for (end, standard) in self.ends() {
            end.get_or_insert_with(|| {
                standard
                    .iter()
                    .filter(|item| !named.contains(item))
                    .cloned()
                    .collect()
            });
        }

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
    let layout = serde_json::from_str::<Layout>(&stripped).map_err(|e| format!("{FILE}: {e}"))?;
    layout.check_items()?;
    Ok(layout.fitted())
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
    if cx.try_global::<Home>().is_none_or(|home| home.dir != dir) {
        cx.set_global(Home {
            dir: dir.to_path_buf(),
            choices: Choices::default(),
        });
    }
    // While a write of ours is on its way the file is behind the window,
    // and read now it would take back what was just done by hand. It is
    // read when the last write is in.
    if cx.try_global::<Writes>().is_some_and(|w| w.pending > 0) {
        return cx
            .try_global::<Failure>()
            .and_then(|failure| failure.0.clone());
    }
    let result = (|| {
        let choices = Choices::read(dir)?;
        let path = layout_path(dir, choices.active.as_deref());
        let source = read(&path, choices.active.is_none())?;
        let layout = parse_at(&path, &source)?;
        layout.check_commands(cx)?;
        let switched = cx.global::<Home>().choices.active != choices.active;
        cx.global_mut::<Home>().choices = choices;
        Ok((layout, switched))
    })();
    match result {
        Ok((layout, switched)) => {
            if switched {
                selected(cx);
            }
            if switched || cx.try_global::<Layout>() != Some(&layout) {
                cx.set_global(layout);
            }
            cx.try_global::<Failure>()
                .and_then(|failure| failure.0.clone())
        }
        Err(error) => {
            if cx.try_global::<Layout>().is_none() {
                cx.set_global(Layout::default());
            }
            Some(error)
        }
    }
}

/// Writes the size `part` has now into the file.
pub fn keep(part: Part, cx: &mut App) {
    let layout = Layout::get(cx);
    let value = match part {
        Part::Left => serde_json::to_value(&layout.left),
        Part::Right => serde_json::to_value(&layout.right),
        Part::Bottom => serde_json::to_value(&layout.bottom),
    };
    if let Ok(value) = value {
        write(Change::Set(part.key(), value), cx);
    }
}

/// The docks were opened, closed or turned to another panel by hand:
/// `open` is what each shows now, and goes into the file.
pub fn keep_open(open: Vec<Panel>, cx: &mut App) {
    // Nothing is kept before a file was looked for: see `Home`.
    if cx.try_global::<Home>().is_none() {
        return;
    }
    let mut layout = Layout::get(cx).clone();
    layout.open = Some(open);
    put(layout.fitted(), cx);
}

/// Puts in use a layout changed by hand, and writes the parts of it that
/// changed into the file.
pub fn put(layout: Layout, cx: &mut App) {
    let old = Layout::get(cx).clone();
    if layout == old {
        return;
    }
    let mut changed = Vec::new();
    if layout.left != old.left {
        changed.push(("left", serde_json::to_value(&layout.left)));
    }
    if layout.right != old.right {
        changed.push(("right", serde_json::to_value(&layout.right)));
    }
    if layout.bottom != old.bottom {
        changed.push(("bottom", serde_json::to_value(&layout.bottom)));
    }
    if layout.hidden != old.hidden {
        changed.push(("hidden", serde_json::to_value(&layout.hidden)));
    }
    if layout.open != old.open {
        changed.push(("open", serde_json::to_value(&layout.open)));
    }
    // A new button's settings must reach the file before a bar names it.
    if layout.items != old.items {
        changed.push(("items", serde_json::to_value(&layout.items)));
    }
    if layout.title_bar != old.title_bar {
        changed.push(("title_bar", serde_json::to_value(&layout.title_bar)));
    }
    if layout.status_bar != old.status_bar {
        changed.push(("status_bar", serde_json::to_value(&layout.status_bar)));
    }
    if layout.tab_bar != old.tab_bar {
        changed.push(("tab_bar", serde_json::to_value(layout.tab_bar)));
    }
    cx.set_global(layout);
    for (key, value) in changed {
        if let Ok(value) = value {
            write(Change::Set(key, value), cx);
        }
    }
}

/// The layout as the editor comes. The file is the truth, so it has to
/// stop saying otherwise: the selected file is put aside with `.old`
/// after its name, with all the user wrote in it.
pub fn reset(cx: &mut App) {
    selected(cx);
    cx.set_global(Layout::default());
    write(Change::Aside, cx);
}

/// What a write does to the file.
enum Change {
    /// Puts one key into it, leaving the rest as the user wrote it. A
    /// file with a mistake in it is left alone: it is the user's to put
    /// right.
    Set(&'static str, serde_json::Value),
    /// Moves it out of the way.
    Aside,
    Choose {
        name: Option<String>,
        projects: Vec<PathBuf>,
    },
    Save {
        name: String,
        layout: Box<Layout>,
        projects: Vec<PathBuf>,
    },
}

impl Change {
    fn make(self, dir: &Path, path: &Path) -> Result<(), String> {
        match self {
            Change::Set(key, value) => {
                let text = read(path, true)?;
                parse_at(path, &text)?;
                let text = if settings::strip_comments(&text).trim().is_empty() {
                    "{\n}\n"
                } else {
                    &text
                };
                let text = import::jsonc::set_key(text, key, &value);
                replace(path, &text)?;
            }
            Change::Aside => {
                match std::fs::rename(path, path.with_extension("json.old")) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(format!("{}: {error}", path.display())),
                }
                // A named layout remains selected, so it needs a file even
                // when it now says only the editor's defaults.
                if path != dir.join(FILE) {
                    replace(path, &file(&Layout::default()))?;
                }
            }
            Change::Choose { name, projects } => {
                check_name(name.as_deref())?;
                let path = layout_path(dir, name.as_deref());
                parse_at(&path, &read(&path, name.is_none())?)?;
                Choices::select(dir, name, projects)?;
            }
            Change::Save {
                name,
                layout,
                projects,
            } => {
                check_name(Some(&name))?;
                // Validate the choices before creating a file; a malformed
                // state is left for the user to repair.
                Choices::read(dir)?;
                let path = layout_path(dir, Some(&name));
                std::fs::create_dir_all(path.parent().unwrap())
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                use std::io::Write;
                let mut target = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                target
                    .write_all(file(&layout).as_bytes())
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                Choices::select(dir, Some(name), projects)?;
            }
        }
        Ok(())
    }
}

fn check_name(name: Option<&str>) -> Result<(), String> {
    if name.is_some_and(|name| !valid_name(name)) {
        Err("layouts: use a name with letters, numbers, spaces, - or _".into())
    } else {
        Ok(())
    }
}

fn read(path: &Path, missing_ok: bool) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if missing_ok && error.kind() == std::io::ErrorKind::NotFound => {
            Ok(String::new())
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn parse_at(path: &Path, source: &str) -> Result<Layout, String> {
    parse(source).map_err(|error| {
        if path.file_name().is_some_and(|name| name == FILE) {
            error
        } else {
            format!(
                "{}: {}",
                path.display(),
                error.trim_start_matches("layout.json: ")
            )
        }
    })
}

fn replace(path: &Path, text: &str) -> Result<(), String> {
    let operation = || -> std::io::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let fresh = path.with_extension("json.new");
        std::fs::write(&fresh, text)?;
        std::fs::rename(fresh, path)
    };
    operation().map_err(|error| format!("{}: {error}", path.display()))
}

/// Makes a change to the file, after the ones that were asked before it.
fn write(change: Change, cx: &mut App) {
    let Some(dir) = cx.try_global::<Home>().map(|home| home.dir.clone()) else {
        return;
    };
    let path = path(cx);
    let writes = cx.default_global::<Writes>();
    writes.pending += 1;
    let before = writes.last.take();
    let background = cx.background_executor().clone();
    let task = cx.spawn(async move |cx| {
        if let Some(before) = before {
            before.await;
        }
        let folder = dir.clone();
        let result = background
            .spawn(async move { change.make(&folder, &path) })
            .await;
        let _ = cx.update(|cx| {
            let error = result.err();
            cx.set_global(Failure(error.clone()));
            let writes = cx.default_global::<Writes>();
            writes.pending = writes.pending.saturating_sub(1);
            // The file now says all that was done by hand, and whatever
            // the user saved in it meanwhile, which was not read then.
            if writes.pending == 0 {
                let error = reload_from(&dir, cx).or(error);
                let errors = &mut cx.default_global::<settings::ConfigErrors>().0;
                errors.retain(|error| {
                    !error.contains("layout.json")
                        && !error.contains("layouts.json")
                        && !error.contains("/layouts/")
                        && !error.starts_with("layouts:")
                });
                errors.extend(error);
            }
            cx.refresh_windows();
        });
    });
    cx.default_global::<Writes>().last = Some(task);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_layouts_keep_their_files_and_remember_projects() {
        let dir = db::testing::dir("named-layouts");
        let default = dir.join(FILE);
        let original = "// common\n{\"left\":{\"width\":320}}\n";
        std::fs::write(&default, original).unwrap();
        let project = dir.join("project");
        let second = dir.join("second");
        let layout =
            parse(r#"{ "right": { "panels": ["files"] }, "open": ["files", "git"] }"#).unwrap();
        Change::Save {
            name: "Review".into(),
            layout: Box::new(layout.clone()),
            projects: vec![project.clone(), second.clone()],
        }
        .make(&dir, &default)
        .unwrap();
        let named = layout_path(&dir, Some("Review"));
        assert_eq!(parse(&read(&named, false).unwrap()).unwrap(), layout);
        assert_eq!(std::fs::read_to_string(&default).unwrap(), original);
        let choices = Choices::read(&dir).unwrap();
        assert_eq!(choices.active.as_deref(), Some("Review"));
        assert_eq!(choices.projects[&project].as_deref(), Some("Review"));
        assert_eq!(choices.projects[&second].as_deref(), Some("Review"));
        // An existing layout is never overwritten by Save as.
        assert!(
            Change::Save {
                name: "Review".into(),
                layout: Box::default(),
                projects: Vec::new()
            }
            .make(&dir, &default)
            .is_err()
        );
        assert_eq!(parse(&read(&named, false).unwrap()).unwrap(), layout);

        // A change by hand belongs to the named file, the common one stays.
        std::fs::write(&named, "// review\n{\"left\":{\"width\":400}}\n").unwrap();
        Change::Set("open", serde_json::json!(["chat"]))
            .make(&dir, &named)
            .unwrap();
        let text = read(&named, false).unwrap();
        assert!(text.contains("// review"));
        assert_eq!(parse(&text).unwrap().open, Some(vec![Panel::Chat]));
        assert_eq!(std::fs::read_to_string(&default).unwrap(), original);
        // A missing or broken choice leaves the selected layout and every
        // project's record alone.
        std::fs::write(dir.join("layouts/Broken.json"), "{broken").unwrap();
        for name in ["Missing", "Broken", "../elsewhere", "Default"] {
            assert!(
                Change::Choose {
                    name: Some(name.into()),
                    projects: vec![project.clone()]
                }
                .make(&dir, &named)
                .is_err()
            );
            assert_eq!(
                Choices::read(&dir).unwrap().active.as_deref(),
                Some("Review")
            );
        }
        assert_eq!(names(&dir), ["Broken", "Review"]);
        // Reset saves the original named file beside a new default layout.
        Change::Aside.make(&dir, &named).unwrap();
        assert_eq!(
            read(&named.with_extension("json.old"), false).unwrap(),
            text
        );
        assert_eq!(
            parse(&read(&named, false).unwrap()).unwrap(),
            Layout::default()
        );
        Change::Choose {
            name: None,
            projects: vec![project.clone()],
        }
        .make(&dir, &named)
        .unwrap();
        let choices = Choices::read(&dir).unwrap();
        assert!(choices.active.is_none() && choices.projects[&project].is_none());
        assert_eq!(choices.projects[&second].as_deref(), Some("Review"));
    }

    #[test]
    fn a_named_layout_cannot_escape_its_folder_or_replace_another_file() {
        for name in [
            "",
            "Default",
            "default",
            "..",
            "../layout",
            "/tmp/layout",
            "dir/name",
            "dir\\name",
            "file.json",
            " trailing ",
            "\n",
            "bad:thing",
        ] {
            assert!(!valid_name(name), "{name}");
        }
        for name in ["Review", "Debugging-2", "My layout", "Обзор", "read_only"] {
            assert!(valid_name(name), "{name}");
        }
        let dir = db::testing::dir("layout-names");
        std::fs::create_dir_all(dir.join("layouts/Folder.json")).unwrap();
        for name in [
            "Review.json",
            "Debugging.json",
            "Review.json.old",
            "Review.json.new",
            "Default.json",
        ] {
            std::fs::write(dir.join("layouts").join(name), "{}").unwrap();
        }
        assert_eq!(names(&dir), ["Debugging", "Review"]);
        std::fs::write(dir.join(CHOICES), r#"{"active":"../settings"}"#).unwrap();
        assert!(Choices::read(&dir).is_err());
    }

    #[gpui::test]
    fn a_selected_named_layout_is_reloaded_when_its_file_is_saved(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let dir = db::testing::dir("watched-named-layout");
        Change::Save {
            name: "Writing".into(),
            layout: Box::default(),
            projects: Vec::new(),
        }
        .make(&dir, &dir.join(FILE))
        .unwrap();
        cx.update(|cx| {
            cx.set_global(crate::perf::Perf::new(std::time::Instant::now()));
            settings::reload_from(&dir, cx);
            settings::watch_dir(dir.clone(), cx);
        });
        let named = layout_path(&dir, Some("Writing"));
        let wait = |cx: &mut gpui::TestAppContext, width: f32, broken: bool| {
            for _ in 0..200 {
                cx.executor()
                    .advance_clock(std::time::Duration::from_millis(50));
                cx.run_until_parked();
                if cx.read(|cx| {
                    Layout::get(cx).left.width == width
                        && cx.global::<settings::ConfigErrors>().0.is_empty() != broken
                }) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("named layout was not reloaded");
        };
        std::fs::write(&named, r#"{"left":{"width":450}}"#).unwrap();
        wait(cx, 450., false);
        std::fs::write(&named, "{broken").unwrap();
        wait(cx, 450., true);
        assert!(cx.read(|cx| {
            cx.global::<settings::ConfigErrors>()
                .0
                .iter()
                .any(|error| error.contains("Writing.json"))
        }));
        std::fs::write(&named, r#"{"left":{"width":500}}"#).unwrap();
        wait(cx, 500., false);
        assert_eq!(cx.read(active), Some("Writing".into()));
    }

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
    fn the_bars_hold_the_items_the_file_names() {
        use Item::*;
        // As they come.
        let standard = Layout::default();
        assert_eq!(standard.title_bar.left(), [Project, Breadcrumbs]);
        assert!(standard.title_bar.right().is_empty());
        assert_eq!(
            standard.status_bar.left(),
            [
                Position, Indent, Encoding, Language, Problems, Activity, Connection
            ]
        );
        assert_eq!(
            standard.status_bar.right(),
            [Extensions, Plugins, Performance]
        );
        assert_eq!(standard.tab_bar.place, TabsAt::Top);
        assert_eq!(parse("{}").unwrap(), standard);
        // A bar's height alone leaves its items as they come.
        let taller = parse(r#"{ "status_bar": { "height": 30 } }"#).unwrap();
        assert_eq!(taller.status_bar.left(), standard.status_bar.left());

        // The file names them: which, in what order, at which end of
        // which bar. An item named twice stays where it was named first,
        // and an end left out keeps what it comes with, less what the
        // file put elsewhere.
        let layout = parse(
            r#"{
              "title_bar": { "left": ["project", "branch"], "right": ["position", "branch"] },
              "status_bar": { "right": ["language", "file"] },
              "tab_bar": { "place": "bottom" }
            }"#,
        )
        .unwrap();
        assert_eq!(layout.title_bar.left(), [Project, Branch]);
        assert_eq!(layout.title_bar.right(), [Position]);
        assert_eq!(
            layout.status_bar.left(),
            [Indent, Encoding, Problems, Activity, Connection]
        );
        assert_eq!(layout.status_bar.right(), [Language, File]);
        assert_eq!(layout.tab_bar.place, TabsAt::Bottom);
        assert_eq!(layout.tab_bar.height, 34.);
        assert_eq!(parse(&file(&layout)).unwrap(), layout);
        // An end with nothing in it is empty, and an item named nowhere
        // is on no bar.
        let bare = parse(
            r#"{ "status_bar": { "left": [], "right": [] }, "tab_bar": { "place": "none" } }"#,
        )
        .unwrap();
        assert!(bare.status_bar.left().is_empty() && bare.status_bar.right().is_empty());
        assert_eq!(bare.title_bar.left(), [Project, Breadcrumbs]);
        assert_eq!(bare.tab_bar.place, TabsAt::None);
        // A name that is no item is a mistake, said with the file's name.
        let error = parse(r#"{ "status_bar": { "left": ["clock"] } }"#).unwrap_err();
        assert!(
            error.starts_with("layout.json") && error.contains("clock"),
            "{error}"
        );
    }

    #[test]
    fn an_item_is_a_word_an_icon_or_both_and_a_button_runs_a_command() {
        use Item::*;
        let button = |name: &str| Button {
            button: name.into(),
        };
        let layout = parse(
            r#"{
              "status_bar": { "left": ["position", { "button": "term" }], "right": ["language"] },
              "items": {
                "position": { "display": "icon" },
                "language": { "display": "text", "icon": "code", "label": "Lang",
                              "command": "workspace::ToggleTerminal" },
                "term": { "label": "Terminal", "icon": "terminal-window",
                          "command": ["workspace::SwitchLayout", { "name": "Review" }] },
                "plug": { "display": "icon", "command": { "plugin": "todo", "command": "list" } }
              }
            }"#,
        )
        .unwrap();
        // A button is an item like the others, by the name it has.
        assert_eq!(layout.status_bar.left(), [Position, button("term")]);
        assert_eq!(layout.item_place(button("term")), Some(BarEnd::StatusLeft));
        // How each is drawn; one the file says nothing of is both.
        assert_eq!(layout.style(&Position).display, Display::Icon);
        assert_eq!(layout.style(&Indent), &ItemStyle::default());
        assert_eq!(ItemStyle::default().display, Display::Both);
        let language = layout.style(&Language);
        assert_eq!(language.label.as_deref(), Some("Lang"));
        assert_eq!(
            language.command,
            Some(Command::Action("workspace::ToggleTerminal".into()))
        );
        // A command is written as in a keymap, or names a plugin's.
        assert!(matches!(
            &layout.style(&button("term")).command,
            Some(Command::Args((name, args)))
                if name == "workspace::SwitchLayout" && args["name"] == "Review"
        ));
        assert!(matches!(
            &layout.style(&button("plug")).command,
            Some(Command::Plugin { plugin, command }) if plugin == "todo" && command == "list"
        ));
        // A button on no bar is hidden, and offered with the hidden ones.
        assert_eq!(layout.item_place(button("plug")), None);
        assert!(layout.named_items().contains(&button("plug")));
        assert_eq!(parse(&file(&layout)).unwrap(), layout);

        // By hand: how an item is drawn, a new button, and its removal.
        let layout = layout.styled(&Position, Display::Text);
        assert_eq!(layout.style(&Position).display, Display::Text);
        let command = Command::Action("workspace::ToggleSidebar".into());
        let layout = layout
            .add_button("Toggle sidebar".into(), command.clone(), BarEnd::TitleRight)
            .add_button("Again".into(), command.clone(), BarEnd::TitleRight);
        assert_eq!(
            layout.title_bar.right(),
            [button("button-1"), button("button-2")]
        );
        let made = layout.style(&button("button-1"));
        assert_eq!(made.label.as_deref(), Some("Toggle sidebar"));
        assert_eq!(made.command, Some(command));
        assert_eq!(parse(&file(&layout)).unwrap(), layout);
        let layout = layout.without_button("button-1");
        assert_eq!(layout.title_bar.right(), [button("button-2")]);
        assert!(!layout.items.contains_key("button-1"));
        assert!(!layout.named_items().contains(&button("button-1")));
        assert_eq!(parse(&file(&layout)).unwrap(), layout);

        // Mistakes are said with the name of what is wrong.
        for (source, says) in [
            // A button with nothing to run.
            (r#"{ "items": { "save": { "label": "Save" } } }"#, "save"),
            (
                r#"{ "items": { "position": { "icon": "nope" } } }"#,
                "unknown icon",
            ),
            (r#"{ "items": { "position": { "label": "  " } } }"#, "label"),
            // A bar names a button `items` does not have.
            (
                r#"{ "status_bar": { "left": [{ "button": "ghost" }] } }"#,
                "ghost",
            ),
            // A button may not take the name of an item of the editor's.
            (
                r#"{ "status_bar": { "left": [{ "button": "position" }] } }"#,
                "position",
            ),
            (r#"{ "status_bar": { "left": [3] } }"#, "item"),
            (
                r#"{ "items": { "position": { "display": "loud" } } }"#,
                "loud",
            ),
        ] {
            let error = parse(source).unwrap_err();
            assert!(
                error.starts_with("layout.json") && error.contains(says),
                "{error}"
            );
        }
    }

    #[test]
    fn bar_items_move_between_ends_without_returning_through_defaults() {
        use BarEnd::*;
        use Item::*;
        let layout = parse(r#"{"title_bar":{"height":46},"status_bar":{"height":32}}"#).unwrap();
        let hidden = layout.clone().hiding_item(Position).hiding_item(Project);
        assert_eq!(hidden.item_place(Position), None);
        assert_eq!(hidden.item_place(Project), None);
        assert_eq!(parse(&file(&hidden)).unwrap(), hidden);

        let moved = hidden
            .moved_item(Project, StatusRight, None)
            .moved_item(Language, TitleLeft, None)
            .moved_item(File, TitleRight, None)
            .moved_item(Position, StatusLeft, Some(Indent));
        assert_eq!(moved.title_bar.left(), [Breadcrumbs, Language]);
        assert_eq!(moved.title_bar.right(), [File]);
        assert_eq!(
            moved.status_bar.right(),
            [Extensions, Plugins, Performance, Project]
        );
        assert_eq!(
            moved.status_bar.left(),
            [Position, Indent, Encoding, Problems, Activity, Connection]
        );
        assert_eq!(moved.title_bar.height, 46.);
        assert_eq!(moved.status_bar.height, 32.);
        assert_eq!(parse(&file(&moved)).unwrap(), moved);
        assert_eq!(
            moved.clone().moved_item(File, TitleRight, Some(File)),
            moved
        );
        let moved = moved.moved_item(Project, TitleRight, Some(File));
        assert_eq!(moved.title_bar.right(), [Project, File]);
        // A stale insertion point is harmless: append instead.
        let moved = moved.moved_item(File, TitleRight, Some(Branch));
        assert_eq!(moved.title_bar.right(), [Project, File]);
        for item in Item::ALL {
            assert!(
                BarEnd::ALL
                    .into_iter()
                    .map(|end| moved
                        .items(end)
                        .iter()
                        .filter(|other| **other == item)
                        .count())
                    .sum::<usize>()
                    <= 1
            );
        }
    }

    #[test]
    fn a_panel_is_moved_and_hidden_by_hand() {
        use Panel::*;
        let standard = Layout::default();
        // To another dock, at the end or before one of its panels.
        let layout = standard.clone().moved(Git, Place::Right, None);
        assert_eq!(layout.right.panels, [Chat, Agent, Git]);
        assert!(!layout.left.panels.contains(&Git));
        let layout = layout.moved(Terminal, Place::Right, Some(Agent));
        assert_eq!(layout.right.panels, [Chat, Terminal, Agent, Git]);
        assert_eq!(layout.bottom.panels, [Problems, Debug, Response, Results]);
        // Along its own dock.
        let layout = layout.moved(Git, Place::Right, Some(Chat));
        assert_eq!(layout.right.panels, [Git, Chat, Terminal, Agent]);
        // Onto itself, or before a panel that is not there: nothing, and
        // the end of the dock.
        assert_eq!(layout.clone().moved(Chat, Place::Right, Some(Chat)), layout);
        let last = layout.clone().moved(Git, Place::Right, Some(Files));
        assert_eq!(last.right.panels, [Chat, Terminal, Agent, Git]);

        // Hidden, it is in no dock; moved to one, it is hidden no more.
        let hidden = layout.hiding(Chat).hiding(Chat);
        assert_eq!(hidden.hidden, [Chat]);
        assert_eq!(hidden.place(Chat), None);
        let back = hidden.moved(Chat, Place::Bottom, None);
        assert!(back.hidden.is_empty());
        assert_eq!(
            back.bottom.panels,
            [Problems, Debug, Response, Results, Chat]
        );
        // What was open goes on being so where it is now.
        let open = parse(r#"{ "open": ["files", "chat"] }"#).unwrap();
        let moved = open.moved(Chat, Place::Bottom, None);
        assert_eq!(moved.open, Some(vec![Files, Chat]));
        assert_eq!(moved.open_in(Place::Bottom), Some(Chat));
        assert_eq!(moved.open_in(Place::Right), None);
    }

    #[test]
    fn each_dock_has_one_panel_open_or_none() {
        use Panel::*;
        // Until the file says, the left dock is open on its first panel.
        let standard = Layout::default();
        assert_eq!(standard.open_in(Place::Left), Some(Files));
        assert_eq!(standard.open_in(Place::Right), None);
        assert_eq!(standard.open_in(Place::Bottom), None);
        let moved = parse(r#"{ "left": { "panels": ["git", "files"] } }"#).unwrap();
        assert_eq!(moved.open_in(Place::Left), Some(Git));
        // What the editor writes for a window nobody touched has no word
        // about it, so it goes on meaning that.
        assert!(!file(&standard).contains("open"));

        // The file names them in any order; the first of a dock is the
        // one, and they are kept in the order of the docks.
        let layout =
            parse(r#"{ "open": ["terminal", "agent", "git", "chat", "files", "git"] }"#).unwrap();
        assert_eq!(layout.open, Some(vec![Git, Agent, Terminal]));
        assert_eq!(layout.open_in(Place::Left), Some(Git));
        assert_eq!(layout.open_in(Place::Right), Some(Agent));
        assert_eq!(layout.open_in(Place::Bottom), Some(Terminal));
        assert_eq!(parse(&file(&layout)).unwrap(), layout);
        // None named: every dock is closed, the left one too.
        let closed = parse(r#"{ "open": [] }"#).unwrap();
        assert_eq!(closed.open, Some(Vec::new()));
        assert_eq!(closed.open_in(Place::Left), None);

        // A panel is open in the dock its tab is in now, and a hidden one
        // in the dock it comes in.
        let layout = parse(
            r#"{
              "right": { "panels": ["files", "chat"] },
              "hidden": ["search"],
              "open": ["files", "search", "agent"]
            }"#,
        )
        .unwrap();
        assert_eq!(layout.open, Some(vec![Search, Files]));
        assert_eq!(layout.open_in(Place::Left), Some(Search));
        assert_eq!(layout.open_in(Place::Right), Some(Files));
    }

    #[test]
    fn every_panel_is_in_one_dock_or_hidden() {
        use Panel::*;
        // As it comes: ten on the left, the chat and the agent on the
        // right, the terminals, the debugger and the two kinds of answers
        // at the bottom, none hidden.
        let standard = Layout::default();
        assert_eq!(
            standard.left.panels,
            [
                Files,
                Search,
                Git,
                Services,
                Database,
                Api,
                Ai,
                Extensions,
                ExtensionViews,
                Tests,
                Structure
            ]
        );
        assert_eq!(standard.right.panels, [Chat, Agent]);
        assert_eq!(
            standard.bottom.panels,
            [Terminal, Problems, Debug, Response, Results]
        );
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
            [
                Chat,
                Git,
                Files,
                Services,
                Database,
                Extensions,
                ExtensionViews,
                Tests,
                Structure
            ]
        );
        assert_eq!(layout.right.panels, [Search, Terminal]);
        assert_eq!(
            layout.bottom.panels,
            [Agent, Problems, Debug, Response, Results]
        );
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
            r#"{ "right": { "panels": ["files", "search", "git", "services", "database", "api", "ai", "extensions", "extension_views", "tests", "structure", "chat", "agent"] } }"#,
        )
        .unwrap();
        assert!(empty.left.panels.is_empty());
        assert_eq!(empty.right.panels.len(), 13);
        assert_eq!(empty.bottom.panels.len(), 5);
    }
}
