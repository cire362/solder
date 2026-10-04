//! The Extensions window: what is installed, and the catalogs of Zed and
//! Open VSX to install from. For each extension it says what Solder takes
//! from it and what it leaves out.

use std::time::Duration;

use extension::{Code, Entry, Extension, Origin, manifest::zed_equivalent};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, SharedString, Subscription, Task, Window, actions, div, prelude::*, px,
    uniform_list,
};

use crate::{
    editor::{Editor, EditorEvent},
    extension_store::{ExtensionStore, key},
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(
    extensions_view,
    [Close, SelectNext, SelectPrevious, Confirm]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Close, Some("ExtensionsView")),
        KeyBinding::new("down", SelectNext, Some("ExtensionsView")),
        KeyBinding::new("up", SelectPrevious, Some("ExtensionsView")),
        KeyBinding::new("enter", Confirm, Some("ExtensionsView")),
    ]);
}

const ROW: gpui::Pixels = px(30.);
/// How long typing must pause before a catalog is asked.
const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Installed,
    Catalog(Origin),
}

/// Where the extension of a row stands.
#[derive(Clone, Debug, PartialEq)]
enum State {
    Available,
    Installed,
    /// Installed, and the catalog has this newer version.
    Update(String),
    Installing(Option<f32>),
}

#[derive(Clone)]
struct Row {
    origin: Origin,
    id: String,
    name: SharedString,
    detail: SharedString,
    state: State,
    /// What to download: the catalog's record, when the row came from one.
    entry: Option<Entry>,
}

pub struct ExtensionsView {
    store: Entity<ExtensionStore>,
    tab: Tab,
    input: Entity<Editor>,
    selected: usize,
    search: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

impl ExtensionsView {
    pub fn new(store: Entity<ExtensionStore>, cx: &mut Context<Self>) -> Self {
        // What is in the folder now, in case it changed since the start.
        store.update(cx, |s, cx| s.scan(cx));
        let input = cx.new(|cx| Editor::single_line("Search", cx));
        let subscriptions = [
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&input, |this, _, event, cx| {
                if let EditorEvent::Edited = event {
                    this.selected = 0;
                    this.search_soon(cx);
                    cx.notify();
                }
            }),
        ];
        Self {
            store,
            tab: Tab::Installed,
            input,
            selected: 0,
            search: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn show(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.selected = 0;
        self.search = None;
        if let Tab::Catalog(origin) = tab {
            let query = self.input.read(cx).text(cx);
            let catalog = self.store.read(cx).catalog(origin);
            // The first look at a catalog, or one made with another query.
            if !catalog.searching && (!catalog.searched || catalog.query != query) {
                self.store
                    .update(cx, |store, cx| store.search(origin, &query, cx));
            }
        }
        cx.notify();
    }

    /// Searches the catalog in front once typing has paused.
    fn search_soon(&mut self, cx: &mut Context<Self>) {
        let Tab::Catalog(origin) = self.tab else {
            return;
        };
        self.search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            this.update(cx, |this, cx| {
                let query = this.input.read(cx).text(cx);
                this.store
                    .update(cx, |store, cx| store.search(origin, &query, cx));
            })
            .ok();
        }));
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        let store = self.store.read(cx);
        let state = |origin: Origin, id: &str, version: Option<&str>| {
            if let Some(progress) = store.installing.get(&key(origin, id)) {
                return State::Installing(progress.fraction());
            }
            match (store.find(origin, id), version) {
                (Some(installed), Some(version)) if installed.version != version => {
                    State::Update(version.to_string())
                }
                (Some(_), _) => State::Installed,
                (None, _) => State::Available,
            }
        };
        match self.tab {
            Tab::Installed => {
                let query = self.input.read(cx).text(cx).to_lowercase();
                store
                    .installed
                    .iter()
                    .filter(|e| {
                        query.is_empty()
                            || e.name.to_lowercase().contains(&query)
                            || e.id.to_lowercase().contains(&query)
                    })
                    .map(|e| Row {
                        origin: e.origin,
                        id: e.id.clone(),
                        name: e.name.clone().into(),
                        detail: format!("{} {}", e.origin.label(), e.version).into(),
                        state: state(e.origin, &e.id, None),
                        entry: None,
                    })
                    .collect()
            }
            Tab::Catalog(origin) => store
                .catalog(origin)
                .entries
                .iter()
                .map(|entry| Row {
                    origin,
                    id: entry.id.clone(),
                    name: entry.name.clone().into(),
                    detail: format!("{}  {}", entry.version, downloads(entry.downloads)).into(),
                    state: state(origin, &entry.id, Some(&entry.version)),
                    entry: Some(entry.clone()),
                })
                .collect(),
        }
    }

    fn close(&mut self, _: &Close, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.rows(cx).len();
        if count > 0 {
            self.selected = (self.selected + 1).min(count - 1);
            cx.notify();
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        cx.notify();
    }

    /// Enter installs or updates the selected extension. Removing takes the
    /// button: it should not happen by a slip of the hand.
    fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.rows(cx).get(self.selected)
            && matches!(row.state, State::Available | State::Update(_))
        {
            self.act(row.clone(), cx);
        }
    }

    /// What the row's button does.
    fn act(&mut self, row: Row, cx: &mut Context<Self>) {
        self.store
            .update(cx, |store, cx| match (&row.state, row.entry) {
                (State::Installing(_), _) => store.cancel(row.origin, &row.id),
                (State::Available | State::Update(_), Some(entry)) => store.install(entry, cx),
                (State::Installed, _) => store.remove(row.origin, &row.id, cx),
                _ => {}
            });
    }

    fn badge(text: SharedString, color: gpui::Hsla, theme: &Theme) -> AnyElement {
        div()
            .flex_none()
            .px_1p5()
            .rounded(px(6.))
            .bg(theme.bg_sunken)
            .text_size(px(11.))
            .text_color(color)
            .child(text)
            .into_any_element()
    }

    /// The selected extension: what it is, what Solder takes from it and
    /// what it does not.
    fn render_details(&self, row: &Row, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let line = |text: SharedString, color: gpui::Hsla| {
            div().text_size(UI_FONT_SIZE).text_color(color).child(text)
        };
        let heading = |text: &'static str| {
            div()
                .pt_1()
                .text_size(px(10.5))
                .text_color(theme.fg_subtle)
                .child(text)
        };
        let mut details = div()
            .flex_1()
            .min_h_0()
            .id("extension-details")
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_1();

        let installed = store.find(row.origin, &row.id);
        let description = match (&row.entry, installed) {
            (Some(entry), _) => entry.description.clone(),
            (None, Some(installed)) => installed.description.clone(),
            (None, None) => String::new(),
        };
        if !description.is_empty() {
            details = details.child(line(description.into(), theme.fg));
        }
        if let Some(error) = store.errors.get(&key(row.origin, &row.id)) {
            details = details.child(line(error.clone().into(), theme.error));
        }

        match installed {
            Some(installed) => {
                details = details
                    .child(heading("SOLDER USES"))
                    .child(line(installed.provides().into(), theme.fg_muted));
                // Said before a file needs it: this is the one thing an
                // extension does outside its sandbox.
                if installed.runs_code() && !installed.servers.is_empty() {
                    details = details.child(line(
                        "Its language server is downloaded and started when a file needs it."
                            .into(),
                        theme.fg_subtle,
                    ));
                }
                let missing = not_running(installed);
                if !missing.is_empty() {
                    details = details.child(heading("DOES NOT RUN HERE"));
                    for what in missing {
                        details = details.child(line(what.into(), theme.fg_muted));
                    }
                }
                if !installed.themes.is_empty() {
                    details = details.child(heading("THEMES"));
                    let view = cx.entity();
                    details = details.child(div().flex().flex_wrap().gap_1().children(
                        installed.themes.iter().enumerate().map(|(i, theme_file)| {
                            let (view, theme_file) = (view.clone(), theme_file.clone());
                            ui::button(
                                ("extension-theme", i),
                                format!("Use {}", theme_file.name),
                                false,
                                theme,
                                move |_, _, cx| {
                                    let theme_file = theme_file.clone();
                                    view.update(cx, |this, cx| {
                                        this.store
                                            .update(cx, |store, cx| store.use_theme(theme_file, cx))
                                    })
                                },
                            )
                            .debug_selector(move || format!("extension-theme-{i}"))
                            .h(px(22.))
                        }),
                    ));
                    match &store.theme_status {
                        Some(Ok(name)) => {
                            details = details.child(line(
                                format!("The theme is now {name}").into(),
                                theme.fg_subtle,
                            ))
                        }
                        Some(Err(error)) => {
                            details = details.child(line(error.clone().into(), theme.error))
                        }
                        None => {}
                    }
                }
            }
            None => {
                if let Some(entry) = &row.entry
                    && !entry.provides.is_empty()
                {
                    details = details.child(heading("HAS")).child(line(
                        entry.provides.join(", ").replace('-', " ").into(),
                        theme.fg_muted,
                    ));
                }
                if row.origin == Origin::VsCode {
                    details = details.child(line(
                        "From a VS Code extension Solder takes themes and snippets. Its code needs VS Code and does not run here.".into(),
                        theme.fg_subtle,
                    ));
                }
            }
        }

        // A VS Code extension for a language: the Zed one brings a grammar
        // and a language server, which this one cannot.
        if row.origin == Origin::VsCode
            && let Some(zed) = zed_equivalent(&row.id)
        {
            let view = cx.entity();
            details = details.child(
                div()
                    .pt_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(line(
                        format!("For the language itself, install {zed} from Zed's catalog.")
                            .into(),
                        theme.fg_muted,
                    ))
                    .child(
                        ui::button(
                            "extension-equivalent",
                            "Find it",
                            false,
                            theme,
                            move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.input
                                        .update(cx, |input, cx| input.set_text(zed, false, cx));
                                    this.show(Tab::Catalog(Origin::Zed), cx);
                                    window.focus(&this.input.focus_handle(cx));
                                })
                            },
                        )
                        .debug_selector(|| "extension-equivalent".into())
                        .h(px(22.)),
                    ),
            );
        }
        details.into_any_element()
    }
}

/// What an installed extension has that Solder does not run, in words.
fn not_running(extension: &Extension) -> Vec<String> {
    let mut missing = extension.missing.clone();
    // A language server is the extension's code at work. Code built for a
    // version of Zed's API this host does not have stays unused.
    if let (false, Code::Zed { api }) = (extension.servers.is_empty(), &extension.code)
        && !extension.runs_code()
    {
        let names: Vec<&str> = extension.servers.iter().map(|s| s.name.as_str()).collect();
        missing.insert(
            0,
            format!(
                "Language server {} (built for Zed's extension API {api})",
                names.join(", ")
            ),
        );
    }
    let plain = extension
        .languages
        .iter()
        .filter(|l| l.grammar.is_none())
        .count();
    if plain > 0 && extension.origin == Origin::Zed {
        missing.push(format!("{plain} of its languages (the grammar is missing)"));
    }
    missing
}

/// `1.2M`, `34K`, `512`: download counts as the catalogs show them.
fn downloads(count: u64) -> String {
    match count {
        0 => String::new(),
        n if n >= 1_000_000 => format!("{:.1}M downloads", n as f64 / 1_000_000.),
        n if n >= 1_000 => format!("{}K downloads", n / 1_000),
        n => format!("{n} downloads"),
    }
}

impl EventEmitter<DismissEvent> for ExtensionsView {}

impl Focusable for ExtensionsView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for ExtensionsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = self.rows(cx);
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        let selected = self.selected;
        let count = rows.len();
        let store = self.store.read(cx);
        let (loaded, folder) = (store.loaded, store.root.clone());
        let empty: SharedString = match self.tab {
            Tab::Installed if !loaded => "Reading the extensions folder...".into(),
            Tab::Installed => {
                "Nothing installed yet. Pick a catalog above to find extensions.".into()
            }
            Tab::Catalog(origin) => {
                let catalog = store.catalog(origin);
                match &catalog.error {
                    Some(error) => error.clone().into(),
                    None if catalog.searching || !catalog.searched => "Searching...".into(),
                    None => "Nothing found".into(),
                }
            }
        };
        let failed =
            matches!(self.tab, Tab::Catalog(origin) if store.catalog(origin).error.is_some());
        let focused = self.input.focus_handle(cx).is_focused(window);
        let details = rows
            .get(selected)
            .map(|row| self.render_details(row, &theme, cx));

        let view = cx.entity();
        let tab_button = |tab: Tab, label: &'static str, id: &'static str| {
            let view = view.clone();
            ui::button(id, label, self.tab == tab, &theme, move |_, _, cx| {
                view.update(cx, |this, cx| this.show(tab, cx))
            })
            .debug_selector(move || id.to_string())
            .h(px(22.))
        };
        let tabs = div()
            .flex()
            .gap_1()
            .child(tab_button(
                Tab::Installed,
                "Installed",
                "extensions-installed",
            ))
            .child(tab_button(
                Tab::Catalog(Origin::Zed),
                "Zed",
                "extensions-zed",
            ))
            .child(tab_button(
                Tab::Catalog(Origin::VsCode),
                "Open VSX",
                "extensions-open-vsx",
            ));

        let list_rows = rows.clone();
        let list_view = view.clone();
        let list = uniform_list("extensions", count, move |range, _, cx| {
            let theme = cx.theme().clone();
            range
                .map(|i| {
                    let row = list_rows[i].clone();
                    let (select, act) = (list_view.clone(), list_view.clone());
                    let (label, primary) = match &row.state {
                        State::Available => ("Install", true),
                        State::Update(_) => ("Update", true),
                        State::Installed => ("Remove", false),
                        State::Installing(_) => ("Cancel", false),
                    };
                    let badge = match &row.state {
                        State::Installed => Some(("Installed".into(), theme.git_added)),
                        State::Update(version) => {
                            Some((format!("{version} available").into(), theme.warning))
                        }
                        State::Installing(Some(done)) => {
                            Some((format!("{:.0}%", done * 100.).into(), theme.fg_muted))
                        }
                        State::Installing(None) => Some(("Downloading".into(), theme.fg_muted)),
                        State::Available => None,
                    };
                    let acted = row.clone();
                    div()
                        .id(("extension", i))
                        .debug_selector(move || format!("extension-{i}"))
                        .w_full()
                        .h(ROW)
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(UI_FONT_SIZE)
                        .when(i == selected, |d| d.bg(theme.bg_elev))
                        .hover(|d| d.bg(theme.bg_elev))
                        .child(div().flex_none().text_color(theme.fg).child(row.name))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_color(theme.fg_subtle)
                                .child(row.detail),
                        )
                        .child(div().flex_1())
                        .children(badge.map(|(text, color)| Self::badge(text, color, &theme)))
                        .child(
                            ui::button(
                                ("extension-act", i),
                                label,
                                primary,
                                &theme,
                                move |_, _, cx| {
                                    let acted = acted.clone();
                                    act.update(cx, |this, cx| {
                                        this.selected = i;
                                        this.act(acted, cx);
                                    })
                                },
                            )
                            .debug_selector(move || format!("extension-act-{i}"))
                            .h(px(22.)),
                        )
                        .on_click(move |_, _, cx| {
                            select.update(cx, |this, cx| {
                                this.selected = i;
                                cx.notify();
                            })
                        })
                })
                .collect()
        })
        .h(ROW * count.clamp(1, 8) as f32);

        div()
            .key_context("ExtensionsView")
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .w(px(640.))
            .max_h(px(540.))
            .flex()
            .flex_col()
            .rounded(px(16.))
            .border_1()
            .border_color(theme.line)
            .bg(theme.bg)
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .h(px(40.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.line)
                    .child(
                        div()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg)
                            .child("Extensions"),
                    )
                    .child(tabs)
                    .child(div().flex_1())
                    .child(
                        ui::button(
                            "extensions-folder",
                            "Open folder",
                            false,
                            &theme,
                            move |_, _, cx| cx.reveal_path(&folder),
                        )
                        .h(px(22.)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .p_2()
                    .flex()
                    .border_b_1()
                    .border_color(theme.line)
                    .child(ui::text_field(self.input.clone(), focused, &theme)),
            )
            .map(|d| {
                if count == 0 {
                    d.child(
                        div()
                            .p_3()
                            .text_size(UI_FONT_SIZE)
                            .text_color(if failed { theme.error } else { theme.fg_subtle })
                            .child(empty),
                    )
                } else {
                    d.child(
                        div()
                            .flex_none()
                            .border_b_1()
                            .border_color(theme.line)
                            .child(list),
                    )
                    .children(details)
                }
            })
    }
}
