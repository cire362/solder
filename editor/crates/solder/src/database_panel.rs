//! The Database tab: detected connections, their tables, collections or
//! keys, and a preview of each in the Results tab.

use std::collections::HashSet;

use db::{Engine, Object, ObjectKind};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    SharedString, Task, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::{
    database::{DatabaseEvent, DatabaseStore, SchemaState, Status},
    picker::{Picker, PickerDelegate},
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(database, [NewConnection, RefreshDatabases]);

pub enum DatabasePanelEvent {
    /// Run `query` on `connection` and show the Results tab.
    Run {
        connection: SharedString,
        query: String,
    },
}

impl EventEmitter<DatabasePanelEvent> for DatabasePanel {}

const ROW_HEIGHT: gpui::Pixels = px(28.);

#[derive(Clone)]
enum Row {
    Connection(usize),
    Object(usize, usize),
    Column(usize, usize, usize),
    Note(SharedString, bool),
    Empty(SharedString),
}

pub struct DatabasePanel {
    store: Entity<DatabaseStore>,
    focus_handle: FocusHandle,
    /// Connection names whose schema is shown.
    expanded: HashSet<String>,
    /// `(connection, object)` names whose columns are shown.
    expanded_objects: HashSet<(String, String)>,
}

impl DatabasePanel {
    pub fn new(store: Entity<DatabaseStore>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&store, |_, _, _: &DatabaseEvent, cx| cx.notify())
            .detach();
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self {
            store,
            focus_handle: cx.focus_handle(),
            expanded: HashSet::new(),
            expanded_objects: HashSet::new(),
        }
    }

    /// Detects connections the first time the tab is shown.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| s.ensure_detected(cx));
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        let store = self.store.read(cx);
        let mut rows = Vec::new();
        if !store.detected() {
            rows.push(Row::Empty("Looking for databases...".into()));
            return rows;
        }
        if store.connections().is_empty() {
            rows.push(Row::Empty(
                "No databases found in .env or compose files. Add one with New connection.".into(),
            ));
        }
        for (i, conn) in store.connections().iter().enumerate() {
            rows.push(Row::Connection(i));
            if !self.expanded.contains(&conn.spec.name) {
                continue;
            }
            if let Status::Failed(e) = &conn.status {
                rows.push(Row::Note(e.clone(), true));
                continue;
            }
            match &conn.schema {
                SchemaState::NotLoaded | SchemaState::Loading => {
                    let text = if conn.status == Status::Connecting {
                        "Connecting..."
                    } else {
                        "Reading schema..."
                    };
                    rows.push(Row::Note(text.into(), false));
                }
                SchemaState::Failed(e) => rows.push(Row::Note(e.clone(), true)),
                SchemaState::Loaded(schema) => {
                    if schema.objects.is_empty() {
                        rows.push(Row::Note("Empty".into(), false));
                    }
                    for (j, object) in schema.objects.iter().enumerate() {
                        rows.push(Row::Object(i, j));
                        let key = (conn.spec.name.clone(), object.name.clone());
                        if self.expanded_objects.contains(&key) {
                            rows.extend((0..object.columns.len()).map(|k| Row::Column(i, j, k)));
                        }
                    }
                    if schema.truncated {
                        rows.push(Row::Note("More keys not listed".into(), false));
                    }
                }
            }
        }
        rows
    }

    pub(crate) fn toggle_connection(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(conn) = self.store.read(cx).connections().get(ix) else {
            return;
        };
        let name = conn.spec.name.clone();
        let load = matches!(conn.schema, SchemaState::NotLoaded | SchemaState::Failed(_))
            || matches!(conn.status, Status::Failed(_));
        if self.expanded.remove(&name) {
            cx.notify();
            return;
        }
        self.expanded.insert(name.clone());
        if load {
            self.store.update(cx, |s, cx| s.load_schema(&name, cx));
        }
        cx.notify();
    }

    fn object(&self, i: usize, j: usize, cx: &App) -> Option<(Engine, SharedString, Object)> {
        let conn = self.store.read(cx).connections().get(i)?;
        let SchemaState::Loaded(schema) = &conn.schema else {
            return None;
        };
        Some((
            conn.spec.engine,
            conn.spec.name.clone().into(),
            schema.objects.get(j)?.clone(),
        ))
    }

    pub(crate) fn open_object(&mut self, i: usize, j: usize, cx: &mut Context<Self>) {
        let Some((engine, connection, object)) = self.object(i, j, cx) else {
            return;
        };
        if !object.columns.is_empty() {
            let key = (connection.to_string(), object.name.clone());
            if !self.expanded_objects.remove(&key) {
                self.expanded_objects.insert(key);
            }
        }
        cx.emit(DatabasePanelEvent::Run {
            connection,
            query: db::preview_query(engine, &object),
        });
        cx.notify();
    }

    fn refresh(&mut self, _: &RefreshDatabases, _: &mut Window, cx: &mut Context<Self>) {
        let expanded: Vec<String> = self.expanded.iter().cloned().collect();
        self.store.update(cx, |s, cx| {
            s.reconnect_all(cx);
            // Reload what is open once detection has run again.
            for name in expanded {
                s.load_schema(&name, cx);
            }
        });
    }

    fn render_row(
        &self,
        ix: usize,
        row: &Row,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let store = self.store.read(cx);
        let base = div()
            .id(ix)
            .h(ROW_HEIGHT)
            .mx_1p5()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(8.))
            .text_size(UI_FONT_SIZE)
            .overflow_hidden();
        match row {
            Row::Empty(text) => base
                .text_color(theme.fg_subtle)
                .child(div().truncate().child(text.clone()))
                .into_any_element(),
            Row::Note(text, error) => base
                .pl(px(26.))
                .text_size(px(11.5))
                .text_color(if *error { theme.error } else { theme.fg_subtle })
                .child(div().truncate().child(text.clone()))
                .into_any_element(),
            Row::Connection(i) => {
                let conn = &store.connections()[*i];
                let open = self.expanded.contains(&conn.spec.name);
                let dot = match conn.status {
                    Status::Idle => theme.fg_subtle,
                    Status::Connecting => theme.warning,
                    Status::Ready => theme.git_added,
                    Status::Failed(_) => theme.error,
                };
                let i = *i;
                base.hover(|d| d.bg(theme.accent_soft))
                    .child(
                        div()
                            .w(px(10.))
                            .flex_none()
                            .text_color(theme.fg_subtle)
                            .child(if open { "▾" } else { "▸" }),
                    )
                    .child(div().size(px(7.)).flex_none().rounded(px(4.)).bg(dot))
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(140.))
                            .truncate()
                            .text_color(theme.fg)
                            .child(conn.spec.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(theme.fg_subtle)
                            .child(conn.spec.engine.label()),
                    )
                    .when(conn.spec.read_only, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .px_1()
                                .rounded(px(6.))
                                .bg(theme.bg_elev)
                                .text_size(px(10.5))
                                .text_color(theme.warning)
                                .child("read-only"),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(theme.fg_subtle)
                            .child(conn.spec.source.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_connection(i, cx)))
                    .into_any_element()
            }
            Row::Object(i, j) => {
                let Some((engine, _, object)) = self.object(*i, *j, cx) else {
                    return base.into_any_element();
                };
                let glyph = match &object.kind {
                    ObjectKind::Table => "T",
                    ObjectKind::View => "V",
                    ObjectKind::Collection => "C",
                    ObjectKind::Key(_) => "K",
                };
                let detail = match &object.kind {
                    ObjectKind::Key(t) => t.clone(),
                    _ => match (&object.namespace, engine) {
                        (Some(ns), Engine::Postgres) if ns != "public" => ns.clone(),
                        (Some(ns), Engine::MySql) => ns.clone(),
                        _ => String::new(),
                    },
                };
                let (i, j) = (*i, *j);
                base.pl(px(26.))
                    .hover(|d| d.bg(theme.accent_soft))
                    .child(
                        div()
                            .flex_none()
                            .w(px(14.))
                            .text_size(px(10.5))
                            .text_color(theme.accent)
                            .font_family(crate::theme::CODE_FONT)
                            .child(glyph),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.fg)
                            .child(object.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(theme.fg_subtle)
                            .child(detail),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.open_object(i, j, cx)))
                    .into_any_element()
            }
            Row::Column(i, j, k) => {
                let Some((_, _, object)) = self.object(*i, *j, cx) else {
                    return base.into_any_element();
                };
                let column = &object.columns[*k];
                base.pl(px(48.))
                    .text_size(px(11.5))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.fg_muted)
                            .child(column.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.fg_subtle)
                            .child(column.type_name.clone()),
                    )
                    .when(column.primary_key, |d| {
                        d.child(div().flex_none().text_color(theme.accent).child("key"))
                    })
                    .into_any_element()
            }
        }
    }
}

impl Focusable for DatabasePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DatabasePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.rows(cx).len();
        div()
            .key_context("DatabasePanel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::refresh))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .pb_2()
                    .flex()
                    .gap_1p5()
                    .child(
                        ui::button(
                            "db-new",
                            "New connection",
                            false,
                            &theme,
                            |_, window, cx| window.dispatch_action(Box::new(NewConnection), cx),
                        )
                        .flex_1(),
                    )
                    .child(ui::button(
                        "db-refresh",
                        "Refresh",
                        false,
                        &theme,
                        |_, window, cx| window.dispatch_action(Box::new(RefreshDatabases), cx),
                    )),
            )
            .child(
                uniform_list(
                    "databases",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let theme = cx.theme().clone();
                        let rows = this.rows(cx);
                        range
                            .filter_map(|ix| {
                                rows.get(ix).map(|r| this.render_row(ix, r, &theme, cx))
                            })
                            .collect()
                    }),
                )
                .flex_1(),
            )
    }
}

/// New connection: paste a URL (or a SQLite file path), confirm, and it is
/// saved to the user's connections file under a name made from the URL.
pub struct NewConnectionPrompt {
    store: Entity<DatabaseStore>,
    url: String,
}

impl NewConnectionPrompt {
    pub fn new(store: Entity<DatabaseStore>) -> Self {
        Self {
            store,
            url: String::new(),
        }
    }

    fn engine(&self) -> Option<Engine> {
        Engine::from_url(&self.url).or_else(|| {
            (self.url.starts_with('/')
                || self.url.ends_with(".db")
                || self.url.ends_with(".sqlite"))
            .then_some(Engine::Sqlite)
        })
    }
}

impl PickerDelegate for NewConnectionPrompt {
    fn placeholder(&self) -> SharedString {
        "postgres://user:password@localhost:5432/app".into()
    }

    fn match_count(&self) -> usize {
        usize::from(self.engine().is_some())
    }

    fn selected_index(&self) -> usize {
        0
    }

    fn set_selected_index(&mut self, _: usize, _: &mut Context<Picker<Self>>) {}

    fn update_matches(
        &mut self,
        query: String,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.url = query.trim().to_string();
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        if self.engine().is_none() {
            return;
        }
        let url = self.url.clone();
        let name = db::suggested_name(&url);
        self.store.update(cx, |s, cx| s.add(name, url, cx)).detach();
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        _: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let engine = self.engine().map_or("", Engine::label);
        div()
            .text_size(UI_FONT_SIZE)
            .text_color(cx.theme().fg)
            .child(format!(
                "Add {engine} connection \u{201c}{}\u{201d}",
                db::suggested_name(&self.url)
            ))
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        "Paste a Postgres, MySQL, Redis or MongoDB URL, or a SQLite file path".into()
    }

    fn width(&self) -> gpui::Pixels {
        px(560.)
    }
}
