//! The structure of one table as a form: columns, indexes and foreign keys.
//! Nothing runs while editing. Review shows the DDL (and the DDL that undoes
//! it); the change is then applied to the database or saved as a migration
//! in the project's own format.

use crate::lang::{t, tf};
use std::path::PathBuf;

use db::{
    Engine,
    ddl::{Change, ColumnDraft, ForeignKeyDraft, IndexDraft, TableDraft},
    migrations,
};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, SharedString, Task,
    Window, actions, div, prelude::*, px,
};

use crate::{
    database::DatabaseStore,
    editor::Editor,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(structure, [ReviewStructure, CloseStructure]);

pub fn bind_keys(cx: &mut App) {
    let context = Some("StructureView");
    cx.bind_keys([
        KeyBinding::new("secondary-s", ReviewStructure, context),
        KeyBinding::new("escape", CloseStructure, context),
    ]);
}

pub enum StructureEvent {
    /// Leave the view (after applying, or when dismissed).
    Close,
    /// A migration was written; open its first file.
    OpenFile(PathBuf),
}

impl EventEmitter<StructureEvent> for StructureView {}

const ON_DELETE: [Option<&str>; 4] = [None, Some("CASCADE"), Some("SET NULL"), Some("RESTRICT")];

struct ColumnRow {
    original: Option<String>,
    name: Entity<Editor>,
    type_name: Entity<Editor>,
    default: Entity<Editor>,
    nullable: bool,
    primary_key: bool,
    auto: bool,
}

struct IndexRow {
    original: Option<String>,
    name: Entity<Editor>,
    columns: Entity<Editor>,
    unique: bool,
}

struct ForeignKeyRow {
    original: Option<String>,
    name: Entity<Editor>,
    columns: Entity<Editor>,
    ref_table: Entity<Editor>,
    ref_columns: Entity<Editor>,
    on_delete: Option<String>,
}

pub struct Review {
    pub up: Vec<String>,
    pub down: Vec<String>,
    pub target: migrations::Target,
}

pub struct StructureView {
    store: Entity<DatabaseStore>,
    root: PathBuf,
    pub connection: SharedString,
    engine: Engine,
    /// The table as it is; `None` while creating one.
    before: Option<TableDraft>,
    name: Entity<Editor>,
    columns: Vec<ColumnRow>,
    indexes: Vec<IndexRow>,
    foreign_keys: Vec<ForeignKeyRow>,
    pub drop: bool,
    pub review: Option<Review>,
    pub notice: Option<SharedString>,
    busy: Option<Task<()>>,
    focus: FocusHandle,
}

fn localized_field(placeholder: &'static str, text: &str, cx: &mut App) -> Entity<Editor> {
    cx.new(|cx| {
        let mut editor = Editor::localized_field(placeholder, cx);
        editor.set_text(text, false, cx);
        editor
    })
}

fn names(list: &[String]) -> String {
    list.join(", ")
}

fn on_delete_label(action: Option<&str>) -> String {
    match action {
        None | Some("NO ACTION") => t("on delete no action").into(),
        Some("CASCADE") => t("on delete cascade").into(),
        Some("SET NULL") => t("on delete set null").into(),
        Some("RESTRICT") => t("on delete restrict").into(),
        Some(other) => tf("on delete {}", &[&other.to_lowercase()]),
    }
}

fn migration_label(tool: migrations::Tool) -> &'static str {
    match tool {
        migrations::Tool::UpDown => t("up/down files"),
        migrations::Tool::Plain => t("SQL file"),
        other => other.label(),
    }
}

fn ddl_subject(what: &str) -> String {
    if let Some(name) = what.strip_prefix("Index ") {
        tf("Index {}", &[&name])
    } else if let Some(name) = what.strip_prefix("Foreign key ") {
        tf("Foreign key {}", &[&name])
    } else {
        what.to_string()
    }
}

/// Validation comes from the database crate; SQL and server errors stay intact.
fn ddl_notice(error: String) -> String {
    match error.as_str() {
        "The table needs a name" => return t("The table needs a name").into(),
        "A table needs at least one column" => {
            return t("A table needs at least one column").into();
        }
        "Every column needs a name" => return t("Every column needs a name").into(),
        "Every index needs a name" => return t("Every index needs a name").into(),
        "Every foreign key needs a name" => return t("Every foreign key needs a name").into(),
        "Structure can be changed for Postgres, MySQL and SQLite" => {
            return t("Structure can be changed for Postgres, MySQL and SQLite").into();
        }
        _ => {}
    }
    if let Some(name) = error.strip_suffix(" needs a type") {
        return tf("{} needs a type", &[&name]);
    }
    if let Some(name) = error.strip_suffix(" appears twice") {
        return tf("{} appears twice", &[&name]);
    }
    if let Some(what) = error.strip_suffix(" needs at least one column") {
        return tf("{} needs at least one column", &[&ddl_subject(what)]);
    }
    if let Some(what) = error.strip_suffix(", which is not a column")
        && let Some((what, column)) = what.split_once(" uses ")
    {
        return tf(
            "{} uses {}, which is not a column",
            &[&ddl_subject(what), &column],
        );
    }
    if let Some(what) = error.strip_suffix(" needs a table to point at") {
        return tf("{} needs a table to point at", &[&ddl_subject(what)]);
    }
    if let Some((what, count)) = error.rsplit_once(" has ")
        && let Some((count, target)) = count.split_once(" columns but points at ")
    {
        return tf(
            "{} has {} columns but points at {}",
            &[&ddl_subject(what), &count, &target],
        );
    }
    error
}

fn database_notice(error: String) -> String {
    if let Some(name) = error.strip_prefix("No connection named ") {
        tf("No connection named {}", &[&name])
    } else {
        error
    }
}

impl StructureView {
    /// Edits `table`, or a new table when `None`.
    pub fn new(
        store: Entity<DatabaseStore>,
        root: PathBuf,
        connection: SharedString,
        engine: Engine,
        table: Option<TableDraft>,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = table.clone().unwrap_or_else(|| TableDraft {
            columns: vec![ColumnDraft {
                name: "id".into(),
                type_name: match engine {
                    Engine::Postgres => "serial",
                    _ => "integer",
                }
                .into(),
                primary_key: true,
                auto: engine == Engine::MySql,
                ..Default::default()
            }],
            ..Default::default()
        });
        let columns = draft
            .columns
            .iter()
            .map(|c| ColumnRow {
                original: c.original.clone(),
                name: localized_field("name", &c.name, cx),
                type_name: localized_field("type", &c.type_name, cx),
                default: localized_field("default", c.default.as_deref().unwrap_or(""), cx),
                nullable: c.nullable,
                primary_key: c.primary_key,
                auto: c.auto,
            })
            .collect();
        let indexes = draft
            .indexes
            .iter()
            .map(|i| IndexRow {
                original: i.original.clone(),
                name: localized_field("name", &i.name, cx),
                columns: localized_field("columns", &names(&i.columns), cx),
                unique: i.unique,
            })
            .collect();
        let foreign_keys = draft
            .foreign_keys
            .iter()
            .map(|fk| ForeignKeyRow {
                original: fk.original.clone(),
                name: localized_field("name", &fk.name, cx),
                columns: localized_field("columns", &names(&fk.columns), cx),
                ref_table: localized_field("table", &fk.ref_table, cx),
                ref_columns: localized_field("columns", &names(&fk.ref_columns), cx),
                on_delete: fk.on_delete.clone(),
            })
            .collect();
        Self {
            store,
            root,
            connection,
            engine,
            name: localized_field("table name", &draft.name, cx),
            before: table,
            columns,
            indexes,
            foreign_keys,
            drop: false,
            review: None,
            notice: None,
            busy: None,
            focus: cx.focus_handle(),
        }
    }

    pub fn title(&self, cx: &App) -> String {
        match &self.before {
            Some(t) => t.name.clone(),
            None => {
                let name = self.name.read(cx).text(cx);
                if name.is_empty() {
                    t("New table").into()
                } else {
                    name
                }
            }
        }
    }

    pub fn add_column(&mut self, cx: &mut Context<Self>) {
        self.columns.push(ColumnRow {
            original: None,
            name: localized_field("name", "", cx),
            type_name: localized_field("type", "", cx),
            default: localized_field("default", "", cx),
            nullable: true,
            primary_key: false,
            auto: false,
        });
        cx.notify();
    }

    pub fn add_index(&mut self, cx: &mut Context<Self>) {
        let table = self.name.read(cx).text(cx);
        self.indexes.push(IndexRow {
            original: None,
            name: localized_field(
                "name",
                &format!("{table}_idx_{}", self.indexes.len() + 1),
                cx,
            ),
            columns: localized_field("columns", "", cx),
            unique: false,
        });
        cx.notify();
    }

    /// Adds a foreign key already filled in (dragged in the ERD).
    pub fn add_foreign_key_draft(&mut self, fk: &ForeignKeyDraft, cx: &mut Context<Self>) {
        self.foreign_keys.push(ForeignKeyRow {
            original: None,
            name: localized_field("name", &fk.name, cx),
            columns: localized_field("columns", &names(&fk.columns), cx),
            ref_table: localized_field("table", &fk.ref_table, cx),
            ref_columns: localized_field("columns", &names(&fk.ref_columns), cx),
            on_delete: fk.on_delete.clone(),
        });
        cx.notify();
    }

    pub fn add_foreign_key(&mut self, cx: &mut Context<Self>) {
        let table = self.name.read(cx).text(cx);
        self.foreign_keys.push(ForeignKeyRow {
            original: None,
            name: localized_field(
                "name",
                &format!("{table}_fk_{}", self.foreign_keys.len() + 1),
                cx,
            ),
            columns: localized_field("columns", "", cx),
            ref_table: localized_field("table", "", cx),
            ref_columns: localized_field("columns", "id", cx),
            on_delete: None,
        });
        cx.notify();
    }

    /// Sets a column's cells: name, type, default.
    #[cfg(test)]
    pub fn set_column(
        &mut self,
        ix: usize,
        name: &str,
        type_name: &str,
        default: &str,
        cx: &mut App,
    ) {
        if let Some(row) = self.columns.get(ix) {
            let (n, t, d) = (row.name.clone(), row.type_name.clone(), row.default.clone());
            n.update(cx, |e, cx| e.set_text(name, false, cx));
            t.update(cx, |e, cx| e.set_text(type_name, false, cx));
            d.update(cx, |e, cx| e.set_text(default, false, cx));
        }
    }

    #[cfg(test)]
    pub fn set_index(&mut self, ix: usize, name: &str, columns: &str, cx: &mut Context<Self>) {
        if let Some(row) = self.indexes.get(ix) {
            let (n, c) = (row.name.clone(), row.columns.clone());
            n.update(cx, |e, cx| e.set_text(name, false, cx));
            c.update(cx, |e, cx| e.set_text(columns, false, cx));
        }
    }

    #[cfg(test)]
    pub fn set_table_name(&mut self, name: &str, cx: &mut Context<Self>) {
        self.name.update(cx, |e, cx| e.set_text(name, false, cx));
    }

    /// The table as the form describes it. Index and key column lists
    /// follow renamed columns, so renaming a column is one edit.
    pub fn draft(&self, cx: &App) -> TableDraft {
        let text = |e: &Entity<Editor>| e.read(cx).text(cx).trim().to_string();
        let columns: Vec<ColumnDraft> = self
            .columns
            .iter()
            .map(|c| ColumnDraft {
                original: c.original.clone(),
                name: text(&c.name),
                type_name: text(&c.type_name),
                nullable: c.nullable,
                default: Some(text(&c.default)).filter(|d| !d.is_empty()),
                primary_key: c.primary_key,
                auto: c.auto,
            })
            .collect();
        let renamed = |name: String| {
            columns
                .iter()
                .find(|c| c.original.as_deref() == Some(name.as_str()))
                .map_or(name, |c| c.name.clone())
        };
        let split = |e: &Entity<Editor>| -> Vec<String> {
            text(e)
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        TableDraft {
            namespace: self.before.as_ref().and_then(|b| b.namespace.clone()),
            name: text(&self.name),
            indexes: self
                .indexes
                .iter()
                .map(|i| IndexDraft {
                    original: i.original.clone(),
                    name: text(&i.name),
                    columns: split(&i.columns).into_iter().map(renamed).collect(),
                    unique: i.unique,
                })
                .collect(),
            foreign_keys: self
                .foreign_keys
                .iter()
                .map(|fk| ForeignKeyDraft {
                    original: fk.original.clone(),
                    name: text(&fk.name),
                    columns: split(&fk.columns).into_iter().map(renamed).collect(),
                    ref_table: text(&fk.ref_table),
                    ref_columns: split(&fk.ref_columns),
                    on_delete: fk.on_delete.clone(),
                })
                .collect(),
            primary_key_name: self
                .before
                .as_ref()
                .and_then(|b| b.primary_key_name.clone()),
            columns,
        }
    }

    pub fn change(&self, cx: &App) -> Change {
        match &self.before {
            None => Change::Create(self.draft(cx)),
            Some(before) if self.drop => Change::Drop(before.clone()),
            Some(before) => Change::Alter {
                before: before.clone(),
                after: self.draft(cx),
            },
        }
    }

    fn review(&mut self, _: &ReviewStructure, window: &mut Window, cx: &mut Context<Self>) {
        let change = self.change(cx);
        let up = match db::ddl::statements(self.engine, &change) {
            Ok(up) if up.is_empty() => {
                self.notice = Some(t("Nothing changed").into());
                return cx.notify();
            }
            Ok(up) => up,
            Err(e) => {
                self.notice = Some(ddl_notice(e).into());
                return cx.notify();
            }
        };
        let down = db::ddl::statements(self.engine, &change.inverse()).unwrap_or_default();
        let root = self.root.clone();
        // Finding the migration folder reads the disk.
        self.busy = Some(cx.spawn_in(window, async move |this, cx| {
            let target = cx
                .background_executor()
                .spawn(async move { migrations::detect(&root) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.notice = None;
                this.review = Some(Review { up, down, target });
                cx.notify();
            })
            .ok();
        }));
    }

    fn close(&mut self, _: &CloseStructure, _: &mut Window, cx: &mut Context<Self>) {
        if self.review.take().is_none() {
            cx.emit(StructureEvent::Close);
        }
        cx.notify();
    }

    pub fn apply(&mut self, cx: &mut Context<Self>) {
        let Some(review) = &self.review else {
            return;
        };
        let name = self.connection.clone();
        let task = self
            .store
            .update(cx, |s, cx| s.apply_ddl(&name, review.up.clone(), cx));
        self.busy = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(()) => {
                        let name = this.connection.clone();
                        this.store.update(cx, |s, cx| s.load_schema(&name, cx));
                        cx.emit(StructureEvent::Close);
                    }
                    Err(e) => this.notice = Some(database_notice(e).into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn save_migration(&mut self, cx: &mut Context<Self>) {
        let Some(review) = &self.review else {
            return;
        };
        let (target, up, down) = (
            review.target.clone(),
            review.up.clone(),
            review.down.clone(),
        );
        let name = match &self.change(cx) {
            Change::Create(t) => format!("create {}", t.name),
            Change::Drop(t) => format!("drop {}", t.name),
            Change::Alter { before, .. } => format!("alter {}", before.name),
        };
        let write = cx
            .background_executor()
            .spawn(async move { migrations::write(&target, &name, &up, &down) });
        self.busy = Some(cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(files) => {
                        if let Some(first) = files.first() {
                            cx.emit(StructureEvent::OpenFile(first.clone()));
                        }
                        cx.emit(StructureEvent::Close);
                    }
                    Err(e) => this.notice = Some(e.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn cell(
        editor: &Entity<Editor>,
        width: f32,
        window: &Window,
        theme: &Theme,
        cx: &App,
    ) -> gpui::Div {
        let focused = editor.focus_handle(cx).is_focused(window);
        div()
            .w(px(width))
            .flex_none()
            .flex()
            .child(ui::text_field(editor.clone(), focused, theme))
    }

    fn section(title: &'static str, theme: &Theme) -> gpui::Div {
        div()
            .pt_4()
            .pb_1p5()
            .text_size(crate::theme::text(11.5))
            .text_color(theme.fg_subtle)
            .child(title)
    }

    fn remove_button(id: impl Into<gpui::ElementId>, theme: &Theme) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex_none()
            .size(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(theme.shape.token)
            .text_color(theme.fg_subtle)
            .hover(|d| d.bg(theme.line).text_color(theme.error))
            .child("×")
    }

    fn render_form(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let columns = self.columns.iter().enumerate().map(|(i, c)| {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .pb_1p5()
                .child(Self::cell(&c.name, 180., window, theme, cx))
                .child(Self::cell(&c.type_name, 180., window, theme, cx))
                .child(Self::cell(&c.default, 180., window, theme, cx))
                .child(ui::toggle(
                    ("required", i),
                    t("required"),
                    "",
                    !c.nullable || c.primary_key,
                    theme,
                    {
                        let view = cx.entity();
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.columns[i].nullable = !this.columns[i].nullable;
                                cx.notify();
                            })
                        }
                    },
                ))
                .child(ui::toggle(
                    ("key", i),
                    t("key"),
                    "",
                    c.primary_key,
                    theme,
                    {
                        let view = cx.entity();
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.columns[i].primary_key = !this.columns[i].primary_key;
                                cx.notify();
                            })
                        }
                    },
                ))
                .when(c.auto, |d| {
                    d.child(
                        div()
                            .text_size(crate::theme::text(11.))
                            .text_color(theme.fg_subtle)
                            .child(t("auto")),
                    )
                })
                .when(c.original.is_none(), |d| {
                    d.child(
                        div()
                            .text_size(crate::theme::text(11.))
                            .text_color(theme.accent)
                            .child(t("new")),
                    )
                })
                .child(div().flex_1())
                .child(
                    Self::remove_button(("remove-column", i), theme).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.columns.remove(i);
                            cx.notify();
                        },
                    )),
                )
        });
        let indexes = self.indexes.iter().enumerate().map(|(i, ix)| {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .pb_1p5()
                .child(Self::cell(&ix.name, 240., window, theme, cx))
                .child(Self::cell(&ix.columns, 300., window, theme, cx))
                .child(ui::toggle(
                    ("unique", i),
                    t("unique"),
                    "",
                    ix.unique,
                    theme,
                    {
                        let view = cx.entity();
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.indexes[i].unique = !this.indexes[i].unique;
                                cx.notify();
                            })
                        }
                    },
                ))
                .child(div().flex_1())
                .child(
                    Self::remove_button(("remove-index", i), theme).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.indexes.remove(i);
                            cx.notify();
                        },
                    )),
                )
        });
        let foreign_keys = self.foreign_keys.iter().enumerate().map(|(i, fk)| {
            let action = on_delete_label(fk.on_delete.as_deref());
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .pb_1p5()
                .child(Self::cell(&fk.name, 200., window, theme, cx))
                .child(Self::cell(&fk.columns, 150., window, theme, cx))
                .child(div().text_color(theme.fg_subtle).child("→"))
                .child(Self::cell(&fk.ref_table, 150., window, theme, cx))
                .child(Self::cell(&fk.ref_columns, 120., window, theme, cx))
                .child(ui::toggle(
                    ("on-delete", i),
                    action,
                    "",
                    fk.on_delete.is_some(),
                    theme,
                    {
                        let view = cx.entity();
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                let current = this.foreign_keys[i].on_delete.as_deref();
                                let at = ON_DELETE.iter().position(|a| *a == current).unwrap_or(0);
                                this.foreign_keys[i].on_delete =
                                    ON_DELETE[(at + 1) % ON_DELETE.len()].map(str::to_string);
                                cx.notify();
                            })
                        }
                    },
                ))
                .child(div().flex_1())
                .child(
                    Self::remove_button(("remove-fk", i), theme).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.foreign_keys.remove(i);
                            cx.notify();
                        },
                    )),
                )
        });
        let view = cx.entity();
        let add = |id: &'static str, label: &'static str, f: fn(&mut Self, &mut Context<Self>)| {
            let view = view.clone();
            ui::button(id, label, false, theme, move |_, _, cx| view.update(cx, f))
        };
        div()
            .flex()
            .flex_col()
            .child(Self::section(t("Columns: name, type, default"), theme))
            .children(columns)
            .child(
                div()
                    .flex()
                    .child(add("add-column", t("Add column"), Self::add_column)),
            )
            .child(Self::section(
                t("Indexes: name, columns separated by commas"),
                theme,
            ))
            .children(indexes)
            .child(
                div()
                    .flex()
                    .child(add("add-index", t("Add index"), Self::add_index)),
            )
            .child(Self::section(
                t("Foreign keys: name, columns → table, columns"),
                theme,
            ))
            .children(foreign_keys)
            .child(
                div()
                    .flex()
                    .child(add("add-fk", t("Add foreign key"), Self::add_foreign_key)),
            )
            .into_any_element()
    }

    fn render_review(&self, review: &Review, theme: &Theme) -> gpui::AnyElement {
        let target = review
            .target
            .dir
            .strip_prefix(&self.root)
            .unwrap_or(&review.target.dir)
            .display()
            .to_string();
        let statements = |list: &[String], color| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .font_family(crate::theme::CODE_FONT)
                .children(
                    list.iter()
                        .map(move |s| div().text_color(color).child(format!("{s};"))),
                )
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(Self::section(t("Runs"), theme))
            .child(statements(&review.up, theme.fg))
            .when(self.engine == Engine::MySql, |d| {
                d.child(
                    div()
                        .text_color(theme.warning)
                        .child(t("MySQL saves each structure change as it runs: if one fails, the ones before it stay applied.")),
                )
            })
            .child(Self::section(t("Undo (the migration's down step)"), theme))
            .child(statements(&review.down, theme.fg_subtle))
            .child(
                div()
                    .text_color(theme.fg_subtle)
                    .child(tf("Save as migration writes a {} to {}.", &[&migration_label(review.target.tool), &target])),
            )
            .into_any_element()
    }
}

impl Focusable for StructureView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for StructureView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let view = cx.entity();
        let busy = self.busy.is_some();
        let body = match &self.review {
            Some(review) => self.render_review(review, &theme),
            None => self.render_form(&theme, window, cx),
        };
        let name_focused = self.name.focus_handle(cx).is_focused(window);
        let buttons = match (&self.review, &self.before) {
            (Some(_), _) => vec![
                ui::button(
                    "structure-back",
                    t("Back"),
                    false,
                    &theme,
                    |_, window, cx| window.dispatch_action(Box::new(CloseStructure), cx),
                ),
                ui::button("structure-save", t("Save as migration"), false, &theme, {
                    let view = view.clone();
                    move |_, _, cx| view.update(cx, |this, cx| this.save_migration(cx))
                }),
                ui::button(
                    "structure-apply",
                    if busy {
                        t("Applying...")
                    } else {
                        t("Apply to database")
                    },
                    true,
                    &theme,
                    {
                        let view = view.clone();
                        move |_, _, cx| view.update(cx, |this, cx| this.apply(cx))
                    },
                ),
            ],
            (None, before) => {
                let mut buttons = Vec::new();
                if before.is_some() {
                    buttons.push(ui::button(
                        "structure-drop",
                        if self.drop {
                            t("Keep table")
                        } else {
                            t("Drop table")
                        },
                        false,
                        &theme,
                        {
                            let view = view.clone();
                            move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.drop = !this.drop;
                                    cx.notify();
                                })
                            }
                        },
                    ));
                }
                buttons.push(ui::button(
                    "structure-review",
                    t("Review"),
                    true,
                    &theme,
                    |_, window, cx| window.dispatch_action(Box::new(ReviewStructure), cx),
                ));
                buttons
            }
        };
        div()
            .id("structure")
            .key_context("StructureView")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::review))
            .on_action(cx.listener(Self::close))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.fg)
            .text_size(UI_FONT_SIZE)
            .child(
                div()
                    .flex_none()
                    .h(px(48.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b(theme.shape.border)
                    .border_color(theme.line)
                    .child(
                        div()
                            .flex_none()
                            .px_1p5()
                            .rounded(theme.shape.token)
                            .bg(theme.bg_elev)
                            .text_color(theme.fg_muted)
                            .child(self.connection.clone()),
                    )
                    .child(div().w(px(260.)).flex().child(ui::text_field(
                        self.name.clone(),
                        name_focused,
                        &theme,
                    )))
                    .when(self.drop, |d| {
                        d.child(div().text_color(theme.error).child(t("will be dropped")))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.error)
                            .children(self.notice.clone()),
                    )
                    .children(buttons),
            )
            .child(
                div()
                    .id("structure-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .pb_4()
                    .child(body),
            )
    }
}

#[cfg(test)]
mod localization_tests {
    use super::*;

    #[test]
    fn russian_structure_labels_leave_sql_and_validation_inputs_unchanged() {
        let draft = TableDraft {
            name: "items".into(),
            columns: vec![ColumnDraft {
                name: "owner_id".into(),
                type_name: "integer".into(),
                ..Default::default()
            }],
            foreign_keys: vec![ForeignKeyDraft {
                name: "owner_fk".into(),
                columns: vec!["owner_id".into()],
                ref_table: "owners".into(),
                ref_columns: vec!["id".into()],
                on_delete: Some("CASCADE".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let english =
            db::ddl::statements(Engine::Postgres, &Change::Create(draft.clone())).unwrap();
        crate::lang::set(crate::lang::Language::Russian);
        assert_eq!(
            on_delete_label(draft.foreign_keys[0].on_delete.as_deref()),
            "при удалении: каскадно"
        );
        assert_eq!(
            db::ddl::statements(Engine::Postgres, &Change::Create(draft.clone())).unwrap(),
            english
        );
        assert!(english.join("\n").contains("ON DELETE CASCADE"));
        let mut invalid = draft;
        invalid.columns[0].type_name.clear();
        let error = invalid.check().unwrap_err();
        assert_eq!(error, "owner_id needs a type");
        assert_eq!(ddl_notice(error), "owner_id: нужен тип");
        assert_eq!(invalid.columns[0].name, "owner_id");
        assert_eq!(
            database_notice("No connection named local-db".into()),
            "Нет подключения с именем local-db"
        );
        assert_eq!(database_notice("Server failure".into()), "Server failure");
        crate::lang::set(crate::lang::Language::English);
        assert_eq!(on_delete_label(Some("CASCADE")), "on delete cascade");
    }
}
