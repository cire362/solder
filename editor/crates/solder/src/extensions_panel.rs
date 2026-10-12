//! The Extensions tab of the sidebar: what is installed and the catalogs of
//! Zed and Open VSX in one list, with one button to install. For each
//! extension it says what Solder takes from it and what it leaves out.
//!
//! The catalogs are asked when the tab is opened and when the search
//! changes, never at startup.

use std::{rc::Rc, time::Duration};

use extension::{Code, Entry, Event, Extension, Origin, Refusals, manifest::zed_equivalent};
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, KeyBinding, SharedString,
    Subscription, Task, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::{
    editor::{Editor, EditorEvent},
    extension_store::{CodeState, ExtensionStore, key},
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(extensions_panel, [SelectNext, SelectPrevious, Confirm]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some("ExtensionsPanel")),
        KeyBinding::new("up", SelectPrevious, Some("ExtensionsPanel")),
        KeyBinding::new("enter", Confirm, Some("ExtensionsPanel")),
    ]);
}

/// Two lines: the name with where it is from, and what it is.
const ROW: gpui::Pixels = px(46.);
/// How long typing must pause before the catalogs are asked.
const DEBOUNCE: Duration = Duration::from_millis(300);
const CATALOGS: [Origin; 2] = [Origin::Zed, Origin::VsCode];
/// How many of a catalog's answers are listed. Zed's answers with a thousand
/// when nothing is searched for; past the first hundred, searching is the
/// way to find one.
const LISTED: usize = 100;

/// Where the extension of a row stands.
#[derive(Clone, Debug, PartialEq)]
enum State {
    Available,
    Installed,
    /// Installed, and a catalog has this newer version.
    Update(String),
    Installing(Option<f32>),
    /// Downloaded, and waiting for the user to allow what it would do
    /// outside a sandbox.
    Review,
}

#[derive(Clone)]
struct Row {
    origin: Origin,
    id: String,
    name: SharedString,
    version: SharedString,
    /// The second line: what it is, or why installing it failed.
    note: SharedString,
    failed: bool,
    /// Installed and turned off.
    off: bool,
    state: State,
    /// What to download: the catalog's record, when a catalog has one.
    entry: Option<Entry>,
}

pub struct ExtensionsPanel {
    store: Entity<ExtensionStore>,
    input: Entity<Editor>,
    selected: usize,
    search: Option<Task<()>>,
    /// The tab was opened and the newest versions were not asked for yet:
    /// that waits for the folder to be read.
    wants_updates: bool,
    _subscriptions: [Subscription; 2],
}

/// The name a catalog goes by in the list.
fn catalog_name(origin: Origin) -> &'static str {
    match origin {
        Origin::Zed => "Zed",
        Origin::VsCode => "Open VSX",
    }
}

impl ExtensionsPanel {
    pub fn new(store: Entity<ExtensionStore>, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| Editor::localized_field("Search extensions", cx));
        let subscriptions = [
            cx.observe(&store, |this, store, cx| {
                if this.wants_updates && store.read(cx).loaded {
                    this.wants_updates = false;
                    store.update(cx, |store, cx| store.check_updates(cx));
                }
                cx.notify()
            }),
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
            input,
            selected: 0,
            search: None,
            wants_updates: false,
            _subscriptions: subscriptions,
        }
    }

    /// The tab came to the front: read the folder again, look for newer
    /// versions of what is in it, and ask the catalogs if they were not
    /// asked this question yet.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        self.wants_updates = true;
        self.store.update(cx, |store, cx| store.scan(cx));
        self.search_now(false, cx);
    }

    /// Asks both catalogs for what is typed. Without `again`, a catalog that
    /// already answered this query is left alone.
    fn search_now(&mut self, again: bool, cx: &mut Context<Self>) {
        self.search = None;
        let query = self.input.read(cx).text(cx);
        self.store.update(cx, |store, cx| {
            for origin in CATALOGS {
                let catalog = store.catalog(origin);
                // Asked or being asked this very question. One still being
                // asked something else is asked again: its answer is dropped.
                let answered = (catalog.searching || catalog.searched) && catalog.query == query;
                if again || !answered {
                    store.search(origin, &query, cx);
                }
            }
        });
    }

    /// Asks the catalogs once typing has paused.
    fn search_soon(&mut self, cx: &mut Context<Self>) {
        self.search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            this.update(cx, |this, cx| this.search_now(false, cx)).ok();
        }));
    }

    /// Puts `query` in the search field and asks for it at once.
    pub fn search_for(&mut self, query: &str, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_text(query, false, cx));
        self.selected = 0;
        self.search_now(false, cx);
        cx.notify();
    }

    /// What is installed that matches the search, then what the catalogs
    /// answered and is not installed, Zed's first.
    fn rows(&self, cx: &App) -> Vec<Row> {
        let store = self.store.read(cx);
        let query = self.input.read(cx).text(cx).to_lowercase();
        let listed = |origin: Origin, id: &str| {
            store
                .catalog(origin)
                .entries
                .iter()
                .find(|entry| entry.id.eq_ignore_ascii_case(id))
        };
        let note = |origin: Origin, id: &str, description: &str| -> (SharedString, bool) {
            match store.errors.get(&key(origin, id)) {
                Some(error) => (crate::extension_store::error_label(error).into(), true),
                None => (description.to_string().into(), false),
            }
        };
        let mut rows = Vec::new();
        for installed in &store.installed {
            if !query.is_empty()
                && !installed.name.to_lowercase().contains(&query)
                && !installed.id.to_lowercase().contains(&query)
            {
                continue;
            }
            let this = key(installed.origin, &installed.id);
            // The newest version: what was asked for when the tab opened,
            // or what a search happens to list. Not for one kept back.
            let kept = store.is_kept(installed.origin, &installed.id);
            let entry = store
                .updates
                .get(&this)
                .or(listed(installed.origin, &installed.id))
                .cloned();
            let state = match (&entry, store.installing.get(&this)) {
                (_, Some(progress)) => State::Installing(progress.fraction()),
                _ if store.pending.contains_key(&this) => State::Review,
                (Some(entry), None) if entry.version != installed.version && !kept => {
                    State::Update(entry.version.clone())
                }
                _ => State::Installed,
            };
            let (note, failed) = note(installed.origin, &installed.id, &installed.description);
            rows.push(Row {
                origin: installed.origin,
                id: installed.id.clone(),
                name: installed.name.clone().into(),
                version: installed.version.clone().into(),
                note,
                failed,
                off: store.is_off(installed.origin, &installed.id),
                state,
                entry,
            });
        }
        // What waits to be allowed stays in the list whatever is searched
        // for: it was downloaded and needs an answer.
        for ((origin, id), staged) in &store.pending {
            if store.find(*origin, id).is_some() {
                continue;
            }
            let waiting = &staged.extension;
            rows.push(Row {
                origin: *origin,
                id: waiting.id.clone(),
                name: waiting.name.clone().into(),
                version: waiting.version.clone().into(),
                note: waiting.description.clone().into(),
                failed: false,
                off: false,
                state: State::Review,
                entry: listed(*origin, id).cloned(),
            });
        }
        // The catalogs' answers in one list. Each catalog orders its own, by
        // how well they match or by downloads when nothing is searched for,
        // and neither order compares across catalogs (Open VSX counts ten
        // times the downloads), so the two take turns, Zed's first: its
        // extensions are the ones that work here in full.
        let mut offered: Vec<(usize, &Entry)> = CATALOGS
            .iter()
            .flat_map(|origin| {
                store
                    .catalog(*origin)
                    .entries
                    .iter()
                    .filter(|entry| store.find(entry.origin, &entry.id).is_none())
                    .filter(|entry| !store.pending.contains_key(&key(entry.origin, &entry.id)))
                    .take(LISTED)
                    .enumerate()
            })
            .collect();
        offered.sort_by_key(|(place, entry)| (*place, entry.origin));
        for (_, entry) in offered {
            let state = match store.installing.get(&key(entry.origin, &entry.id)) {
                Some(progress) => State::Installing(progress.fraction()),
                None => State::Available,
            };
            let (note, failed) = note(entry.origin, &entry.id, &entry.description);
            rows.push(Row {
                origin: entry.origin,
                id: entry.id.clone(),
                name: entry.name.clone().into(),
                version: entry.version.clone().into(),
                note,
                failed,
                off: false,
                state,
                entry: Some(entry.clone()),
            });
        }
        rows
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
                // Waiting to be allowed: the answer is given below the
                // list, next to what it asks for.
                _ => {}
            });
    }

    fn badge(text: SharedString, color: gpui::Hsla, theme: &Theme) -> AnyElement {
        div()
            .flex_none()
            .px_1p5()
            .rounded(theme.shape.token)
            .bg(theme.bg)
            .text_size(crate::theme::text(11.))
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
                .text_size(crate::theme::text(10.5))
                .text_color(theme.fg_subtle)
                .child(text)
        };
        let mut details = div()
            .id("extension-details")
            .flex_none()
            .max_h(px(280.))
            .overflow_y_scroll()
            .border_t(theme.shape.border)
            .border_color(theme.line)
            .p_3()
            .flex()
            .flex_col()
            .gap_1();

        let installed = store.find(row.origin, &row.id);
        let description = match (&row.entry, installed) {
            (_, Some(installed)) => installed.description.clone(),
            (Some(entry), None) => entry.description.clone(),
            (None, None) => String::new(),
        };
        if !description.is_empty() {
            details = details.child(line(description.into(), theme.fg));
        }
        if let Some(error) = store.errors.get(&key(row.origin, &row.id)) {
            details = details.child(line(
                crate::extension_store::error_label(error).into(),
                theme.error,
            ));
        }

        // What a downloaded extension would do outside a sandbox, and the
        // two answers. Nothing of it is in place until Install is pressed.
        let asks = store.asks(row.origin, &row.id);
        if !asks.is_empty() {
            details = details.child(heading(crate::lang::t("INSTALLING IT LETS IT")));
            for what in asks {
                details = details.child(line(permission_label(&what).into(), theme.fg));
            }
            let (allow, refuse) = (cx.entity(), cx.entity());
            let (origin, id) = (row.origin, row.id.clone());
            let refused = id.clone();
            details = details.child(
                div()
                    .pt_1()
                    .flex()
                    .gap_1()
                    .child(
                        ui::button(
                            "extension-allow",
                            crate::lang::t("Install"),
                            true,
                            theme,
                            move |_, _, cx| {
                                allow.update(cx, |this, cx| {
                                    this.store
                                        .update(cx, |store, cx| store.allow(origin, &id, cx))
                                })
                            },
                        )
                        .debug_selector(|| "extension-allow".into())
                        .h(px(22.)),
                    )
                    .child(
                        ui::button(
                            "extension-refuse",
                            crate::lang::t("Cancel"),
                            false,
                            theme,
                            move |_, _, cx| {
                                refuse.update(cx, |this, cx| {
                                    this.store
                                        .update(cx, |store, cx| store.refuse(origin, &refused, cx))
                                })
                            },
                        )
                        .debug_selector(|| "extension-refuse".into())
                        .h(px(22.)),
                    ),
            );
        }

        // A VS Code extension for a language: the Zed one brings a grammar
        // and a language server, which this one cannot. Said first, where
        // it is seen without scrolling: it is what to do next.
        if row.origin == Origin::VsCode
            && let Some(zed) = zed_equivalent(&row.id)
        {
            let view = cx.entity();
            // The sentence, then the button under it: side by side they do
            // not fit a sidebar.
            details = details
                .child(
                    div().pt_1().child(line(
                        crate::lang::tf(
                            "For the language itself, install {} from Zed's catalog.",
                            &[&zed],
                        )
                        .into(),
                        theme.fg_muted,
                    )),
                )
                .child(
                    div().flex().child(
                        ui::button(
                            "extension-equivalent",
                            crate::lang::t("Find it"),
                            false,
                            theme,
                            move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.search_for(zed, cx);
                                    window.focus(&this.input.focus_handle(cx));
                                })
                            },
                        )
                        .debug_selector(|| "extension-equivalent".into())
                        .h(px(22.)),
                    ),
                );
        }

        match installed {
            Some(installed) => {
                let off = store.is_off(installed.origin, &installed.id);
                let kept = store.is_kept(installed.origin, &installed.id);
                let (toggle, keep, remove) = (cx.entity(), cx.entity(), cx.entity());
                let (origin, id) = (installed.origin, installed.id.clone());
                let (keep_id, remove_id) = (id.clone(), id.clone());
                details = details.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .child(
                            ui::button(
                                "extension-off",
                                if off {
                                    crate::lang::t("Turn on")
                                } else {
                                    crate::lang::t("Turn off")
                                },
                                false,
                                theme,
                                move |_, _, cx| {
                                    toggle.update(cx, |this, cx| {
                                        this.store.update(cx, |store, cx| {
                                            store.set_off(origin, &id, !off, cx)
                                        })
                                    })
                                },
                            )
                            .debug_selector(|| "extension-off".into())
                            .h(px(22.)),
                        )
                        .child(
                            ui::button(
                                "extension-keep",
                                if kept {
                                    crate::lang::t("Follow updates")
                                } else {
                                    crate::lang::t("Keep this version")
                                },
                                false,
                                theme,
                                move |_, _, cx| {
                                    keep.update(cx, |this, cx| {
                                        this.store.update(cx, |store, cx| {
                                            store.set_kept(origin, &keep_id, !kept, cx)
                                        })
                                    })
                                },
                            )
                            .debug_selector(|| "extension-keep".into())
                            .h(px(22.)),
                        )
                        // The row's button removes too, but it becomes Update
                        // when there is one; here Remove is always at hand.
                        .child(
                            ui::button(
                                "extension-remove",
                                crate::lang::t("Remove"),
                                false,
                                theme,
                                move |_, _, cx| {
                                    remove.update(cx, |this, cx| {
                                        this.store.update(cx, |store, cx| {
                                            store.remove(origin, &remove_id, cx)
                                        })
                                    })
                                },
                            )
                            .debug_selector(|| "extension-remove".into())
                            .h(px(22.)),
                        ),
                );
                if off {
                    details = details.child(line(
                        crate::lang::t(
                            "Turned off: its languages, snippets and servers are not used.",
                        )
                        .into(),
                        theme.fg_subtle,
                    ));
                }
                details = details
                    .child(heading(crate::lang::t("SOLDER USES")))
                    .child(line(provides_label(installed).into(), theme.fg_muted));
                // Said before a file needs it: this is the one thing an
                // extension does outside its sandbox.
                if installed.runs_code() && !installed.servers.is_empty() {
                    details = details.child(line(
                        crate::lang::t(
                            "Its language server is downloaded and started when a file needs it.",
                        )
                        .into(),
                        theme.fg_subtle,
                    ));
                }
                // What its code may do outside its sandbox, each to take
                // back or give again, and what it did since the app started.
                if installed.origin == Origin::Zed && installed.runs_code() {
                    let refused = store.refusals(&installed.id);
                    let did = store.did(&installed.id);
                    let view = cx.entity();
                    let id = installed.id.clone();
                    let may = |name: &'static str,
                               index: usize,
                               what: String,
                               no: bool,
                               change: Rc<dyn Fn(&mut Refusals)>| {
                        let (view, id, refused) = (view.clone(), id.clone(), refused.clone());
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(UI_FONT_SIZE)
                                    .text_color(if no { theme.fg_subtle } else { theme.fg_muted })
                                    .child(what),
                            )
                            .child(
                                ui::button(
                                    (name, index),
                                    if no {
                                        crate::lang::t("Allow")
                                    } else {
                                        crate::lang::t("Refuse")
                                    },
                                    false,
                                    theme,
                                    move |_, _, cx| {
                                        let mut refused = refused.clone();
                                        change(&mut refused);
                                        let id = id.clone();
                                        view.update(cx, |this, cx| {
                                            this.store.update(cx, |store, cx| {
                                                store.set_refusals(&id, refused, cx)
                                            })
                                        })
                                    },
                                )
                                .debug_selector(move || format!("{name}-{index}"))
                                .h(px(20.)),
                            )
                    };
                    details = details.child(heading(crate::lang::t("IT MAY")));
                    if !installed.commands.is_empty() {
                        details = details.child(may(
                            "extension-may-run",
                            0,
                            crate::lang::t("Run the commands it declares").into(),
                            refused.commands,
                            Rc::new(|r| r.commands = !r.commands),
                        ));
                    }
                    details = details
                        .child(may(
                            "extension-may-npm",
                            0,
                            crate::lang::t("Install packages from npm").into(),
                            refused.npm,
                            Rc::new(|r| r.npm = !r.npm),
                        ))
                        .child(may(
                            "extension-may-download",
                            0,
                            crate::lang::t("Download files").into(),
                            refused.downloads,
                            Rc::new(|r| r.downloads = !r.downloads),
                        ));
                    // The hosts it went to, and those it may not go to.
                    let mut hosts = refused.hosts.clone();
                    for event in &did {
                        if let Event::Downloaded(host) = event
                            && !hosts.contains(host)
                        {
                            hosts.push(host.clone());
                        }
                    }
                    hosts.sort();
                    if !refused.downloads {
                        for (i, host) in hosts.into_iter().enumerate() {
                            let no = refused.hosts.contains(&host);
                            let toggled = host.clone();
                            details = details.child(may(
                                "extension-may-host",
                                i,
                                crate::lang::tf("Download from {}", &[&host]),
                                no,
                                Rc::new(move |r| {
                                    match r.hosts.iter().position(|h| *h == toggled) {
                                        Some(at) => {
                                            r.hosts.remove(at);
                                        }
                                        None => r.hosts.push(toggled.clone()),
                                    }
                                }),
                            ));
                        }
                    }
                    if !did.is_empty() {
                        details =
                            details.child(heading(crate::lang::t("SINCE SOLDER STARTED, IT")));
                        for event in did {
                            let (text, color) = match event {
                                Event::Ran(command) => {
                                    (crate::lang::tf("Ran {}", &[&command]), theme.fg_muted)
                                }
                                Event::Installed(package) => (
                                    crate::lang::tf("Installed {} from npm", &[&package]),
                                    theme.fg_muted,
                                ),
                                Event::Downloaded(host) => (
                                    crate::lang::tf("Downloaded from {}", &[&host]),
                                    theme.fg_muted,
                                ),
                                Event::Refused(what) => (
                                    crate::lang::tf("Was refused: {}", &[&permission_label(&what)]),
                                    theme.warning,
                                ),
                                Event::Failed(what) => (
                                    crate::lang::tf(
                                        "Failed: {}",
                                        &[&crate::extension_store::error_label(&what)],
                                    ),
                                    theme.error,
                                ),
                            };
                            details = details.child(line(text.into(), color));
                        }
                    }
                }
                // The code of a VS Code extension: whether it runs, and
                // what it asked of VS Code's API that is not here.
                if installed.node().is_some() {
                    details = details.child(heading(crate::lang::t("ITS CODE")));
                    let asks = store.asks_installed(installed.origin, &installed.id);
                    let code = store.code(&installed.id);
                    let (view, id) = (cx.entity(), installed.id.clone());
                    if !asks.is_empty() {
                        details = details.child(line(
                            crate::lang::t("Waits for you to allow it. It would:").into(),
                            theme.fg_muted,
                        ));
                        for what in asks {
                            details = details.child(line(permission_label(&what).into(), theme.fg));
                        }
                        details = details.child(
                            div().flex().child(
                                ui::button(
                                    "extension-allow-code",
                                    crate::lang::t("Allow"),
                                    false,
                                    theme,
                                    move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.store.update(cx, |store, cx| {
                                                store.allow(Origin::VsCode, &id, cx)
                                            })
                                        })
                                    },
                                )
                                .debug_selector(|| "extension-allow-code".into())
                                .h(px(22.)),
                            ),
                        );
                    } else if !off {
                        match code.map(|code| &code.state) {
                            None => {
                                details = details.child(line(
                                    crate::lang::t("Starts when it is needed.").into(),
                                    theme.fg_subtle,
                                ))
                            }
                            Some(CodeState::Starting) => {
                                details = details.child(line(
                                    crate::lang::t("Starting...").into(),
                                    theme.fg_muted,
                                ))
                            }
                            Some(CodeState::Running) => {
                                let commands = code.map_or(0, |code| code.commands.len());
                                let text = match commands {
                                    0 => crate::lang::t("Running").to_string(),
                                    1 => crate::lang::t("Running, with 1 command").to_string(),
                                    n => crate::lang::tf("Running, with {} commands", &[&n]),
                                };
                                details = details.child(line(text.into(), theme.fg_muted));
                            }
                            Some(CodeState::Stopped(why)) => {
                                details = details
                                    .child(line(
                                        crate::lang::tf(
                                            "Stopped: {}",
                                            &[&crate::extension_store::error_label(why)],
                                        )
                                        .into(),
                                        theme.error,
                                    ))
                                    .child(
                                        div().flex().child(
                                            ui::button(
                                                "extension-restart-code",
                                                crate::lang::t("Start again"),
                                                false,
                                                theme,
                                                move |_, _, cx| {
                                                    view.update(cx, |this, cx| {
                                                        this.store.update(cx, |store, cx| {
                                                            store.restart_code(&id, cx)
                                                        })
                                                    })
                                                },
                                            )
                                            .debug_selector(|| "extension-restart-code".into())
                                            .h(px(22.)),
                                        ),
                                    );
                            }
                        }
                    }
                    if let Some(code) = code {
                        // What it answers for as a language server would.
                        if !code.languages.is_empty() {
                            let every = code.languages.iter().any(|language| language == "*");
                            let of = match every {
                                true => crate::lang::t("every language").to_string(),
                                false => code.languages.join(", "),
                            };
                            details = details.child(line(
                                crate::lang::tf("Gives language features for {}", &[&of]).into(),
                                theme.fg_muted,
                            ));
                        }
                        if !code.missing.is_empty() {
                            let mut names =
                                code.missing.iter().take(12).cloned().collect::<Vec<_>>();
                            if code.missing.len() > names.len() {
                                names.push(crate::lang::tf(
                                    "{} more",
                                    &[&(code.missing.len() - names.len())],
                                ));
                            }
                            details = details.child(line(
                                crate::lang::tf(
                                    "Asked for what Solder does not have yet: {}",
                                    &[&names.join(", ")],
                                )
                                .into(),
                                theme.fg_subtle,
                            ));
                        }
                        if let Some(last) =
                            code.said.back().and_then(|(_, text)| text.lines().next())
                        {
                            details = details.child(line(
                                crate::lang::tf("Last said: {}", &[&last]).into(),
                                theme.fg_subtle,
                            ));
                        }
                    }
                    // What it wrote to its output channels, to read in a tab.
                    let channels = store.outputs(&installed.id);
                    if !channels.is_empty() {
                        let view = cx.entity();
                        details = details.child(div().flex().flex_wrap().gap_1().children(
                            channels.into_iter().enumerate().map(|(i, channel)| {
                                let (view, id) = (view.clone(), installed.id.clone());
                                let label = crate::lang::tf("Show output: {}", &[&channel]);
                                ui::button(
                                    ("extension-output", i),
                                    label,
                                    false,
                                    theme,
                                    move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.store.update(cx, |store, cx| {
                                                store.show_output(&id, &channel, cx)
                                            })
                                        })
                                    },
                                )
                                .debug_selector(move || format!("extension-output-{i}"))
                                .h(px(22.))
                            }),
                        ));
                    }
                }
                let missing = not_running(installed);
                if !missing.is_empty() {
                    details = details.child(heading(crate::lang::t("DOES NOT RUN HERE")));
                    for what in missing {
                        details = details.child(line(what.into(), theme.fg_muted));
                    }
                }
                if !installed.settings.is_empty() {
                    details = details.child(heading(crate::lang::t("SETTINGS, IN SETTINGS.JSON")));
                    // What each is now: the user's value, or what the
                    // extension says it is when not set.
                    for setting in installed.settings.iter().take(40) {
                        let now = store.configuration(&setting.key, cx);
                        let value = match now {
                            serde_json::Value::Null => crate::lang::t("not set").to_string(),
                            value => value.to_string(),
                        };
                        let own = now_set(&setting.key, cx);
                        details = details.child(line(
                            format!("{}: {value}", setting.key).into(),
                            if own { theme.fg } else { theme.fg_muted },
                        ));
                    }
                    if installed.settings.len() > 40 {
                        details = details.child(line(
                            crate::lang::tf("and {} more", &[&(installed.settings.len() - 40)])
                                .into(),
                            theme.fg_subtle,
                        ));
                    }
                }
                if !installed.themes.is_empty() {
                    details = details.child(heading(crate::lang::t("THEMES")));
                    let view = cx.entity();
                    details = details.child(div().flex().flex_wrap().gap_1().children(
                        installed.themes.iter().enumerate().map(|(i, theme_file)| {
                            let (view, theme_file) = (view.clone(), theme_file.clone());
                            ui::button(
                                ("extension-theme", i),
                                crate::lang::tf("Use {}", &[&theme_file.name]),
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
                                crate::lang::tf("The theme is now {}", &[&name]).into(),
                                theme.fg_subtle,
                            ))
                        }
                        Some(Err(error)) => {
                            details = details.child(line(
                                crate::extension_store::error_label(error).into(),
                                theme.error,
                            ))
                        }
                        None => {}
                    }
                }
                if !installed.icon_themes.is_empty() {
                    details = details.child(heading(crate::lang::t("ICON THEMES")));
                    let view = cx.entity();
                    details = details.child(div().flex().flex_wrap().gap_1().children(
                        installed.icon_themes.iter().enumerate().map(|(i, icons)| {
                            let (view, name) = (view.clone(), icons.name.clone());
                            ui::button(
                                ("extension-icons", i),
                                crate::lang::tf("Use {}", &[&name]),
                                false,
                                theme,
                                move |_, _, cx| {
                                    let name = name.clone();
                                    view.update(cx, |this, cx| {
                                        this.store
                                            .update(cx, |store, cx| store.use_icon_theme(name, cx))
                                    })
                                },
                            )
                            .debug_selector(move || format!("extension-icons-{i}"))
                            .h(px(22.))
                        }),
                    ));
                    match &store.icon_status {
                        Some(Ok(name)) => {
                            details = details.child(line(
                                crate::lang::tf("File icons are now {}", &[&name]).into(),
                                theme.fg_subtle,
                            ))
                        }
                        Some(Err(error)) => {
                            details = details.child(line(
                                crate::extension_store::error_label(error).into(),
                                theme.error,
                            ))
                        }
                        None => {}
                    }
                }
            }
            None => {
                if let Some(entry) = &row.entry
                    && !entry.provides.is_empty()
                {
                    details = details.child(heading(crate::lang::t("HAS"))).child(line(
                        entry
                            .provides
                            .iter()
                            .map(|part| capability_label(part))
                            .collect::<Vec<_>>()
                            .join(", ")
                            .into(),
                        theme.fg_muted,
                    ));
                }
                if row.origin == Origin::VsCode {
                    details = details.child(line(
                        crate::lang::t("From a VS Code extension Solder takes themes, snippets and languages. Its code runs with the parts of VS Code's API Solder has.").into(),
                        theme.fg_subtle,
                    ));
                }
            }
        }

        details.into_any_element()
    }
}

/// Approval text is an English identifier in state.json. Translate only its display.
fn permission_label(text: &str) -> String {
    match text {
        "Run its code with Node.js, outside a sandbox" => {
            return crate::lang::t("Run its code with Node.js, outside a sandbox").into();
        }
        "Draw notebook outputs with browser code" => {
            return crate::lang::t("Draw notebook outputs with browser code").into();
        }
        _ => {}
    }
    if let Some(name) = text.strip_prefix("Download and start the language server ") {
        return crate::lang::tf("Download and start the language server {}", &[&name]);
    }
    if let Some(name) = text.strip_prefix("Get and start the debug adapter ") {
        return crate::lang::tf("Get and start the debug adapter {}", &[&name]);
    }
    if let Some(name) = text.strip_prefix("Get and start the context server ") {
        return crate::lang::tf("Get and start the context server {}", &[&name]);
    }
    if let Some(host) = text.strip_prefix("Download from ") {
        return crate::lang::tf("Download from {}", &[&host]);
    }
    if let Some(package) = text
        .strip_prefix("Install ")
        .and_then(|package| package.strip_suffix(" from npm"))
    {
        return crate::lang::tf("Install {} from npm", &[&package]);
    }
    if let Some(command) = text.strip_prefix("Run ") {
        if let Some(command) = command.strip_suffix(", which its manifest does not declare") {
            return crate::lang::tf("Run {}, which its manifest does not declare", &[&command]);
        }
        return crate::lang::tf("Run {}", &[&command]);
    }
    text.to_string()
}

fn provides_label(extension: &Extension) -> String {
    if crate::lang::current() == crate::lang::Language::English {
        return extension.provides();
    }
    let runs = extension.runs_code();
    let parts = [
        (
            extension
                .languages
                .iter()
                .filter(|language| language.grammar.is_some())
                .count(),
            crate::lang::t("Languages"),
        ),
        (
            if runs { extension.servers.len() } else { 0 },
            crate::lang::t("Language servers"),
        ),
        (
            if runs {
                extension.debug_adapters.len()
            } else {
                0
            },
            crate::lang::t("Debug adapters"),
        ),
        (
            if runs {
                extension.context_servers.len()
            } else {
                0
            },
            crate::lang::t("Context servers"),
        ),
        (extension.debuggers.len(), crate::lang::t("Debuggers")),
        (extension.contributed.len(), crate::lang::t("Commands")),
        (extension.keys.len(), crate::lang::t("Key bindings")),
        (extension.themes.len(), crate::lang::t("Themes")),
        (extension.icon_themes.len(), crate::lang::t("Icon themes")),
        (extension.snippets.len(), crate::lang::t("Snippet files")),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, label)| format!("{label}: {count}"))
    .collect::<Vec<_>>();
    if parts.is_empty() {
        crate::lang::t("Nothing Solder can use").into()
    } else {
        parts.join(", ")
    }
}

fn capability_label(capability: &str) -> String {
    if crate::lang::current() == crate::lang::Language::English {
        return capability.replace('-', " ");
    }
    match capability {
        "languages" => crate::lang::t("Languages").into(),
        "language-servers" => crate::lang::t("Language servers").into(),
        "themes" => crate::lang::t("Themes").into(),
        "icon-themes" => crate::lang::t("Icon themes").into(),
        "debug-adapters" => crate::lang::t("Debug adapters").into(),
        "context-servers" => crate::lang::t("Context servers").into(),
        "slash-commands" => crate::lang::t("Slash commands").into(),
        "agent-servers" => crate::lang::t("Agent servers").into(),
        "indexed-docs-providers" => crate::lang::t("Documentation indexing").into(),
        _ => capability.replace('-', " "),
    }
}

fn missing_label(text: &str) -> String {
    match text {
        "Slash commands" => return crate::lang::t("Slash commands").into(),
        "Agent servers" => return crate::lang::t("Agent servers").into(),
        "Documentation indexing" => return crate::lang::t("Documentation indexing").into(),
        "Its code (the file it starts from is not there)" => {
            return crate::lang::t("Its code (the file it starts from is not there)").into();
        }
        "Its code, which is made for a browser" => {
            return crate::lang::t("Its code, which is made for a browser").into();
        }
        "An icon theme (drawn with a font, or not readable)" => {
            return crate::lang::t("An icon theme (drawn with a font, or not readable)").into();
        }
        "A debugger its code starts" => return crate::lang::t("A debugger its code starts").into(),
        "Product icon themes" => return crate::lang::t("Product icon themes").into(),
        "A key binding (a key Solder cannot bind)" => {
            return crate::lang::t("A key binding (a key Solder cannot bind)").into();
        }
        _ => {}
    }
    if let Some(count) = text.strip_suffix(" of its themes (could not be read)") {
        return crate::lang::tf("{} of its themes (could not be read)", &[&count]);
    }
    if let Some(count) = text.strip_suffix(" icon themes (drawn with a font, or not readable)") {
        return crate::lang::tf(
            "{} icon themes (drawn with a font, or not readable)",
            &[&count],
        );
    }
    if let Some(count) = text.strip_suffix(" debuggers its code starts") {
        return crate::lang::tf("{} debuggers its code starts", &[&count]);
    }
    if let Some(count) = text.strip_suffix(" key bindings (keys Solder cannot bind)") {
        return crate::lang::tf("{} key bindings (keys Solder cannot bind)", &[&count]);
    }
    text.to_string()
}

/// What an installed extension has that Solder does not run, in words.
fn not_running(extension: &Extension) -> Vec<String> {
    let mut missing = extension
        .missing
        .iter()
        .map(|text| missing_label(text))
        .collect::<Vec<_>>();
    // A language server is the extension's code at work. Code built for a
    // version of Zed's API this host does not have stays unused.
    if let (false, Code::Zed { api }) = (extension.servers.is_empty(), &extension.code)
        && !extension.runs_code()
    {
        let names: Vec<&str> = extension.servers.iter().map(|s| s.name.as_str()).collect();
        missing.insert(
            0,
            crate::lang::tf(
                "Language server {} (built for Zed's extension API {})",
                &[&names.join(", "), &api],
            ),
        );
    }
    let plain = extension
        .languages
        .iter()
        .filter(|l| l.grammar.is_none())
        .count();
    if plain > 0 && extension.origin == Origin::Zed {
        missing.push(crate::lang::tf(
            "{} of its languages (the grammar is missing)",
            &[&plain],
        ));
    }
    missing
}

/// `1.2M`, `34K`, `512`: download counts as the catalogs show them.
fn downloads(count: u64) -> String {
    match count {
        0 => String::new(),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.),
        n if n >= 1_000 => format!("{}K", n / 1_000),
        n => n.to_string(),
    }
}

impl Focusable for ExtensionsPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for ExtensionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = self.rows(cx);
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        let selected = self.selected;
        let count = rows.len();
        let store = self.store.read(cx);
        let folder = store.root.clone();
        let updates = store.updates.len();
        let update_all = self.store.clone();

        // What the catalogs are doing, under the list: one that is being
        // asked, and one that could not be.
        let searching = CATALOGS
            .iter()
            .any(|origin| store.catalog(*origin).searching);
        let failures: Vec<SharedString> = CATALOGS
            .iter()
            .filter_map(|origin| {
                let error = store.catalog(*origin).error.as_ref()?;
                Some(format!("{}: {error}", catalog_name(*origin)).into())
            })
            .collect();
        let empty: Option<SharedString> = (count == 0).then(|| {
            if !store.loaded || searching {
                crate::lang::t("Looking...").into()
            } else if failures.is_empty() {
                crate::lang::t("Nothing found").into()
            } else {
                crate::lang::t("Nothing to show").into()
            }
        });
        let focused = self.input.focus_handle(cx).is_focused(window);
        let details = rows
            .get(selected)
            .map(|row| self.render_details(row, &theme, cx));

        let downloads_of: Vec<SharedString> = rows
            .iter()
            .map(|row| {
                row.entry
                    .as_ref()
                    .map(|entry| downloads(entry.downloads))
                    .unwrap_or_default()
                    .into()
            })
            .collect();
        let view = cx.entity();
        let list = uniform_list("extensions", count, move |range, _, cx| {
            let theme = cx.theme().clone();
            range
                .map(|i| {
                    let row = rows[i].clone();
                    let (select, act) = (view.clone(), view.clone());
                    let (label, primary) = match &row.state {
                        State::Available => (crate::lang::t("Install"), true),
                        State::Update(_) => (crate::lang::t("Update"), true),
                        State::Installed => (crate::lang::t("Remove"), false),
                        State::Installing(_) => (crate::lang::t("Cancel"), false),
                        State::Review => (crate::lang::t("Review"), true),
                    };
                    let badge: Option<(SharedString, gpui::Hsla)> = match &row.state {
                        State::Installed if row.off => {
                            Some((crate::lang::t("Off").into(), theme.fg_subtle))
                        }
                        State::Installed => {
                            Some((crate::lang::t("Installed").into(), theme.git_added))
                        }
                        State::Review => Some((crate::lang::t("Asks first").into(), theme.warning)),
                        State::Update(version) => Some((
                            crate::lang::tf("{} is out", &[&version]).into(),
                            theme.warning,
                        )),
                        State::Installing(Some(done)) => {
                            Some((format!("{:.0}%", done * 100.).into(), theme.fg_muted))
                        }
                        State::Installing(None) => {
                            Some((crate::lang::t("Downloading").into(), theme.fg_muted))
                        }
                        State::Available => None,
                    };
                    let source: SharedString = match row.origin {
                        Origin::Zed => "Zed".into(),
                        Origin::VsCode => "VS Code".into(),
                    };
                    let acted = row.clone();
                    div()
                        .id(("extension", i))
                        .debug_selector(move || format!("extension-{i}"))
                        .w_full()
                        .h(crate::theme::row(ROW, cx))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(i == selected, |d| d.bg(theme.bg_elev))
                        .hover(|d| d.bg(theme.bg_elev))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .text_size(UI_FONT_SIZE)
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(theme.fg)
                                                .child(row.name),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_size(crate::theme::text(11.))
                                                .text_color(theme.fg_subtle)
                                                .child(source),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_size(crate::theme::text(11.))
                                                .text_color(theme.fg_subtle)
                                                .child(row.version),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_size(crate::theme::text(11.))
                                                .text_color(theme.fg_subtle)
                                                .child(downloads_of[i].clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(crate::theme::text(11.5))
                                        .text_color(if row.failed {
                                            theme.error
                                        } else {
                                            theme.fg_subtle
                                        })
                                        .child(row.note),
                                ),
                        )
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
                            .flex_none()
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
        .size_full();

        div()
            .key_context("ExtensionsPanel")
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .pb_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(ui::text_field(self.input.clone(), focused, &theme))
                    .when(updates > 0, |d| {
                        d.child(
                            ui::button(
                                "extensions-update-all",
                                crate::lang::tf("Update {}", &[&updates]),
                                true,
                                &theme,
                                move |_, _, cx| {
                                    update_all.update(cx, |store, cx| store.update_all(cx))
                                },
                            )
                            .debug_selector(|| "extensions-update-all".into())
                            .flex_none()
                            .h(px(28.)),
                        )
                    })
                    .child(
                        ui::button(
                            "extensions-folder",
                            crate::lang::t("Folder"),
                            false,
                            &theme,
                            move |_, _, cx| cx.reveal_path(&folder),
                        )
                        .flex_none()
                        .h(px(28.)),
                    ),
            )
            .child(div().flex_1().min_h_0().map(|d| {
                match empty {
                    Some(text) => d.child(
                        div()
                            .p_3()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg_subtle)
                            .child(text),
                    ),
                    None => d.child(list),
                }
            }))
            .children(failures.into_iter().map(|failure| {
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_size(crate::theme::text(11.5))
                    .text_color(theme.error)
                    .child(failure)
            }))
            .children(details)
    }
}

/// Whether settings.json sets `key` itself, written with the dots or as
/// objects inside objects.
fn now_set(key: &str, cx: &App) -> bool {
    let Some(settings) = cx.try_global::<crate::settings::Settings>() else {
        return false;
    };
    if settings.other.contains_key(key) {
        return true;
    }
    let mut parts = key.split('.');
    let first = parts.next().unwrap_or_default();
    settings
        .other
        .get(first)
        .and_then(|inside| parts.try_fold(inside, |at, part| at.get(part)))
        .is_some_and(|value| !value.is_null())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_and_capability_display_leave_their_english_identifiers() {
        let approval = "Run gem install solargraph";
        let capability = "language-servers";
        crate::lang::set(crate::lang::Language::Russian);
        assert_eq!(
            permission_label(approval),
            "Запускать gem install solargraph"
        );
        assert_eq!(approval, "Run gem install solargraph");
        assert_eq!(capability_label(capability), "Языковые серверы");
        assert_eq!(capability, "language-servers");
        assert_eq!(permission_label("provider text"), "provider text");
        crate::lang::set(crate::lang::Language::English);
        assert_eq!(permission_label(approval), approval);
        assert_eq!(capability_label(capability), "language servers");
    }
}
