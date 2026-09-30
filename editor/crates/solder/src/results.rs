//! The Results tab in the bottom dock: the last query's rows in a grid.
//! Rows are virtualized; columns pan together with the header.
//!
//! Rows from a single table with a primary key can be edited. Changes are
//! staged in the grid, reviewed as the SQL that will run, and applied in one
//! transaction; a row that changed since it was read aborts the whole save.

use std::{ops::Range, sync::Arc};

use db::{
    QueryResult, Value,
    edit::{Changes, EditTarget},
};
use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, KeyBinding, MouseButton, Pixels,
    ScrollStrategy, ScrollWheelEvent, SharedString, Subscription, Task, UniformListScrollHandle,
    Window, actions, canvas, div, prelude::*, px, uniform_list,
};

use crate::{
    database::{self, DatabaseStore, SchemaState},
    editor::Editor,
    settings::Settings,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(
    results,
    [
        SelectUp,
        SelectDown,
        SelectLeft,
        SelectRight,
        CopyCell,
        EditCell,
        CancelEdit,
        SetNull,
        DeleteRow,
        ReviewChanges,
        DiscardChanges,
        ApplyChanges,
        EditInline,
        InsertRow,
        DuplicateRow,
    ]
);

/// Asks the workspace to open a picker of the rows a foreign key points at.
pub enum ResultsEvent {
    PickReference {
        row: usize,
        column: usize,
        connection: SharedString,
        query: String,
        title: SharedString,
    },
}

impl gpui::EventEmitter<ResultsEvent> for ResultsView {}

/// What a cell shows: the value read, a staged value, or (new rows only)
/// the column's default.
enum Shown {
    Read(Value),
    Staged(Option<String>),
    Default,
}

pub fn bind_keys(cx: &mut App) {
    let context = Some("ResultsGrid");
    cx.bind_keys([
        KeyBinding::new("up", SelectUp, context),
        KeyBinding::new("down", SelectDown, context),
        KeyBinding::new("left", SelectLeft, Some("ResultsGrid && !editing")),
        KeyBinding::new("right", SelectRight, Some("ResultsGrid && !editing")),
        KeyBinding::new("secondary-c", CopyCell, Some("ResultsGrid && !editing")),
        // Enter reaches the grid from the single-line cell editor too.
        KeyBinding::new("enter", EditCell, context),
        // F2 always types the value, even where Enter would open a picker.
        KeyBinding::new("f2", EditInline, context),
        KeyBinding::new("secondary-n", InsertRow, Some("ResultsGrid && !editing")),
        KeyBinding::new("secondary-d", DuplicateRow, Some("ResultsGrid && !editing")),
        KeyBinding::new("escape", CancelEdit, context),
        KeyBinding::new("shift-backspace", SetNull, Some("ResultsGrid && !editing")),
        KeyBinding::new(
            "secondary-backspace",
            DeleteRow,
            Some("ResultsGrid && !editing"),
        ),
        KeyBinding::new("secondary-s", ReviewChanges, context),
    ]);
}

struct Editing {
    row: usize,
    column: usize,
    editor: Entity<Editor>,
    _blur: Subscription,
}

const ROW_HEIGHT: Pixels = px(24.);
const MIN_COLUMN: f32 = 64.;
const MAX_COLUMN: f32 = 420.;
/// Characters shown in a cell; the full value is one copy away.
const CELL_CHARS: usize = 200;

pub enum State {
    Empty,
    Running,
    Done(Arc<QueryResult>),
    Failed(SharedString),
}

pub struct ResultsView {
    store: Entity<DatabaseStore>,
    focus: FocusHandle,
    pub connection: SharedString,
    pub query: SharedString,
    pub state: State,
    widths: Vec<Pixels>,
    number_width: Pixels,
    scroll: UniformListScrollHandle,
    pan: Pixels,
    viewport: Pixels,
    pub selected: Option<(usize, usize)>,
    task: Option<Task<()>>,
    pub changes: Changes,
    editing: Option<Editing>,
    /// Showing the SQL the staged changes will run.
    pub reviewing: bool,
    /// Why editing is not possible, or why saving failed.
    pub notice: Option<SharedString>,
    /// The notice is about a read-only connection that can be unlocked.
    locked_notice: bool,
    applying: Option<Task<()>>,
}

impl ResultsView {
    pub fn new(store: Entity<DatabaseStore>, cx: &mut Context<Self>) -> Self {
        Self {
            store,
            focus: cx.focus_handle(),
            connection: SharedString::default(),
            query: SharedString::default(),
            state: State::Empty,
            widths: Vec::new(),
            number_width: px(0.),
            scroll: UniformListScrollHandle::new(),
            pan: px(0.),
            viewport: px(0.),
            selected: None,
            task: None,
            changes: Changes::default(),
            editing: None,
            reviewing: false,
            notice: None,
            locked_notice: false,
            applying: None,
        }
    }

    /// Runs `query` on the named connection and shows what comes back.
    pub fn run(&mut self, connection: SharedString, query: String, cx: &mut Context<Self>) {
        self.connection = connection.clone();
        self.query = query.clone().into();
        self.state = State::Running;
        self.selected = None;
        self.pan = px(0.);
        self.changes = Changes::default();
        self.editing = None;
        self.reviewing = false;
        self.notice = None;
        self.locked_notice = false;
        let task = self
            .store
            .update(cx, |store, cx| store.run(&connection, query, cx));
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(result) => {
                        let char_width = Settings::get(cx).buffer_font_size() * 0.6;
                        this.widths = column_widths(&result, char_width);
                        this.number_width =
                            char_width * result.rows.len().to_string().len() as f32 + px(20.);
                        State::Done(Arc::new(result))
                    }
                    Err(e) => State::Failed(e.into()),
                };
                this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn result(&self) -> Option<&Arc<QueryResult>> {
        match &self.state {
            State::Done(r) => Some(r),
            _ => None,
        }
    }

    fn content_width(&self) -> Pixels {
        self.widths
            .iter()
            .fold(self.number_width, |sum, w| sum + *w)
    }

    fn pan_by(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        let limit = (self.content_width() - self.viewport).max(px(0.));
        self.pan = (self.pan + delta).clamp(px(0.), limit);
        cx.notify();
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(ROW_HEIGHT);
        if delta.x != px(0.) {
            self.pan_by(-delta.x, cx);
        }
    }

    fn select(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.commit_edit(cx);
        self.selected = Some((row, column));
        self.scroll.scroll_to_item(row, ScrollStrategy::Top);
        // Keep the selected column on screen.
        let left = self.number_width + self.widths[..column].iter().fold(px(0.), |sum, w| sum + *w);
        let right = left + self.widths[column];
        if left - self.number_width < self.pan {
            self.pan = left - self.number_width;
        } else if right > self.pan + self.viewport {
            self.pan = (right - self.viewport).max(px(0.));
        }
        cx.notify();
    }

    fn move_selection(&mut self, rows: isize, columns: isize, cx: &mut Context<Self>) {
        let Some(result) = self.result() else {
            return;
        };
        if result.columns.is_empty() || self.row_count() == 0 {
            return;
        }
        let (row, column) = self.selected.unwrap_or((0, 0));
        let row = row.saturating_add_signed(rows).min(self.row_count() - 1);
        let column = column
            .saturating_add_signed(columns)
            .min(result.columns.len() - 1);
        self.select(row, column, cx);
    }

    fn select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.move_selection(-1, 0, cx);
    }
    fn select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.move_selection(1, 0, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.move_selection(0, -1, cx);
    }
    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.move_selection(0, 1, cx);
    }

    fn copy_cell(&mut self, _: &CopyCell, _: &mut Window, cx: &mut Context<Self>) {
        let value = self
            .selected
            .map(|(row, column)| match self.shown(row, column) {
                Shown::Read(Value::Null) | Shown::Staged(None) | Shown::Default => String::new(),
                Shown::Read(value) => value.display(),
                Shown::Staged(Some(text)) => text,
            });
        if let Some(value) = value {
            cx.write_to_clipboard(ClipboardItem::new_string(value));
        }
    }

    /// Rows read plus rows added.
    fn row_count(&self) -> usize {
        self.result().map_or(0, |r| r.rows.len()) + self.changes.inserted.len()
    }

    /// Which added row `row` is, if it is one.
    fn new_row(&self, row: usize) -> Option<usize> {
        row.checked_sub(self.result()?.rows.len())
    }

    fn shown(&self, row: usize, column: usize) -> Shown {
        if let Some(new) = self.new_row(row) {
            return match self.changes.inserted.get(new).and_then(|r| r.get(&column)) {
                Some(value) => Shown::Staged(value.clone()),
                None => Shown::Default,
            };
        }
        if let Some(staged) = self.changes.cells.get(&(row, column)) {
            return Shown::Staged(staged.clone());
        }
        Shown::Read(
            self.result()
                .and_then(|r| r.rows.get(row)?.get(column).cloned())
                .unwrap_or(Value::Null),
        )
    }

    /// Stages `value` for a cell (`None` is NULL), in a read row or an
    /// added one.
    pub fn stage(
        &mut self,
        row: usize,
        column: usize,
        value: Option<String>,
        cx: &mut Context<Self>,
    ) {
        match self.new_row(row) {
            Some(new) => {
                if let Some(cells) = self.changes.inserted.get_mut(new) {
                    cells.insert(column, value);
                }
            }
            None => {
                let original = self
                    .result()
                    .and_then(|r| r.rows.get(row)?.get(column))
                    .cloned();
                let unchanged = match (&original, &value) {
                    (Some(Value::Null), None) => true,
                    (Some(Value::Null), Some(_)) => false,
                    (Some(read), Some(text)) => read.display() == *text,
                    _ => false,
                };
                if unchanged {
                    self.changes.cells.remove(&(row, column));
                } else {
                    self.changes.cells.insert((row, column), value);
                }
            }
        }
        cx.notify();
    }

    fn insert_row(&mut self, _: &InsertRow, window: &mut Window, cx: &mut Context<Self>) {
        match self.edit_target(cx) {
            Ok(target) => {
                self.set_notice(None, cx);
                self.changes.inserted.push(Default::default());
                self.start_new_row(&target, window, cx);
            }
            Err(reason) => self.set_notice(Some(reason), cx),
        }
    }

    /// A copy of the selected row, leaving out what the server fills in.
    fn duplicate_row(&mut self, _: &DuplicateRow, window: &mut Window, cx: &mut Context<Self>) {
        let Some((row, _)) = self.selected else {
            return;
        };
        let target = match self.edit_target(cx) {
            Ok(target) => target,
            Err(reason) => return self.set_notice(Some(reason), cx),
        };
        let copy = match self.new_row(row) {
            Some(new) => {
                let mut cells = self.changes.inserted[new].clone();
                cells.retain(|c, _| !target.auto.contains(c));
                cells
            }
            None => {
                let Some(values) = self.result().and_then(|r| r.rows.get(row)).cloned() else {
                    return;
                };
                let mut cells = db::edit::duplicate(&target, &values);
                // Edits staged on the source row come along.
                for ((r, c), v) in &self.changes.cells {
                    if *r == row && !target.auto.contains(c) {
                        cells.insert(*c, v.clone());
                    }
                }
                cells
            }
        };
        self.set_notice(None, cx);
        self.changes.inserted.push(copy);
        self.start_new_row(&target, window, cx);
    }

    /// Selects the last added row at its first column the server does not
    /// fill in, and starts typing there.
    fn start_new_row(&mut self, target: &EditTarget, window: &mut Window, cx: &mut Context<Self>) {
        let columns = self.result().map_or(0, |r| r.columns.len());
        let column = (0..columns).find(|c| !target.auto.contains(c)).unwrap_or(0);
        let row = self.row_count() - 1;
        self.select(row, column, cx);
        self.edit_inline(&EditInline, window, cx);
    }

    /// The table these rows can be saved to, or why they cannot.
    fn edit_target(&mut self, cx: &mut Context<Self>) -> Result<EditTarget, SharedString> {
        let Some(result) = self.result().cloned() else {
            return Err("Nothing to edit".into());
        };
        let store = self.store.read(cx);
        let Some(conn) = store.connection(&self.connection) else {
            return Err("The connection is gone".into());
        };
        let engine = conn.spec.engine;
        let locked = conn.locked();
        match &conn.schema {
            SchemaState::Loaded(schema) => {
                let target = db::edit::edit_target(engine, schema, &self.query, &result.columns)?;
                if locked {
                    self.locked_notice = true;
                    return Err(
                        "This connection is read-only. Allow changes to edit its rows.".into(),
                    );
                }
                Ok(target)
            }
            SchemaState::Failed(e) => Err(e.clone()),
            SchemaState::NotLoaded | SchemaState::Loading => {
                let name = self.connection.clone();
                self.store.update(cx, |s, cx| s.ensure_schema(&name, cx));
                Err("Reading the schema, try again in a moment".into())
            }
        }
    }

    fn set_notice(&mut self, notice: Option<SharedString>, cx: &mut Context<Self>) {
        if notice.is_none() {
            self.locked_notice = false;
        }
        self.notice = notice;
        cx.notify();
    }

    /// Enter: types the value, or for a foreign key column picks one of the
    /// rows it can point at.
    fn edit_cell(&mut self, _: &EditCell, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.commit_edit(cx);
            window.focus(&self.focus);
            return;
        }
        let Some((row, column)) = self.selected else {
            return;
        };
        let target = match self.edit_target(cx) {
            Ok(target) => target,
            Err(_) => return self.edit_inline(&EditInline, window, cx),
        };
        let reference = target.references.iter().find(|(c, _)| *c == column);
        let schema = self.store.read(cx).schema_of(&self.connection);
        match (reference, schema) {
            (Some((_, fk)), Some((engine, schema))) if !self.changes.deleted.contains(&row) => {
                cx.emit(ResultsEvent::PickReference {
                    row,
                    column,
                    connection: self.connection.clone(),
                    query: db::edit::reference_query(engine, &schema, fk),
                    title: format!("{}.{}", fk.ref_table, fk.ref_columns[0]).into(),
                });
            }
            _ => self.edit_inline(&EditInline, window, cx),
        }
    }

    fn edit_inline(&mut self, _: &EditInline, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.commit_edit(cx);
            window.focus(&self.focus);
            return;
        }
        let Some((row, column)) = self.selected else {
            return;
        };
        if self.changes.deleted.contains(&row) {
            return;
        }
        self.locked_notice = false;
        if let Err(reason) = self.edit_target(cx) {
            self.notice = Some(reason);
            cx.notify();
            return;
        }
        self.notice = None;
        let current = match self.shown(row, column) {
            Shown::Read(Value::Null) | Shown::Staged(None) | Shown::Default => String::new(),
            Shown::Read(value) => value.display(),
            Shown::Staged(Some(text)) => text,
        };
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line("NULL", cx);
            editor.set_text(&current, true, cx);
            editor
        });
        let focus = editor.focus_handle(cx);
        // Clicking elsewhere keeps what was typed.
        let blur = cx.on_blur(&focus, window, |this, _, cx| this.commit_edit(cx));
        window.focus(&focus);
        self.editing = Some(Editing {
            row,
            column,
            editor,
            _blur: blur,
        });
        cx.notify();
    }

    /// Stages what the cell editor holds. An unchanged value stages nothing,
    /// and an empty cell in a new row keeps its default.
    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let text = editing.editor.read(cx).text(cx);
        if text.is_empty() && matches!(self.shown(editing.row, editing.column), Shown::Default) {
            cx.notify();
            return;
        }
        self.stage(editing.row, editing.column, Some(text), cx);
    }

    fn cancel_edit(&mut self, _: &CancelEdit, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            window.focus(&self.focus);
        } else if self.reviewing {
            self.reviewing = false;
        } else {
            cx.propagate();
        }
        cx.notify();
    }

    fn set_null(&mut self, _: &SetNull, _: &mut Window, cx: &mut Context<Self>) {
        let Some((row, column)) = self.selected else {
            return;
        };
        if let Err(reason) = self.edit_target(cx) {
            return self.set_notice(Some(reason), cx);
        }
        self.stage(row, column, None, cx);
        self.set_notice(None, cx);
    }

    /// Marks the selected row for deletion, or unmarks it.
    fn delete_row(&mut self, _: &DeleteRow, _: &mut Window, cx: &mut Context<Self>) {
        let Some((row, _)) = self.selected else {
            return;
        };
        if let Err(reason) = self.edit_target(cx) {
            return self.set_notice(Some(reason), cx);
        }
        // An added row is simply dropped.
        if let Some(new) = self.new_row(row) {
            self.changes.inserted.remove(new);
            let count = self.row_count();
            self.selected =
                (count > 0).then(|| (row.min(count - 1), self.selected.map_or(0, |s| s.1)));
            return self.set_notice(None, cx);
        }
        if !self.changes.deleted.remove(&row) {
            self.changes.deleted.insert(row);
        }
        self.set_notice(None, cx);
    }

    /// The statements the staged changes run, in order.
    pub fn statements(&mut self, cx: &mut Context<Self>) -> Result<Vec<String>, SharedString> {
        let target = self.edit_target(cx)?;
        let result = self.result().cloned().ok_or("Nothing to edit")?;
        let engine = self
            .store
            .read(cx)
            .engine_of(&self.connection)
            .ok_or("The connection is gone")?;
        Ok(db::edit::statements(
            engine,
            &target,
            &result.columns,
            &result.rows,
            &self.changes,
        ))
    }

    fn review(&mut self, _: &ReviewChanges, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(cx);
        window.focus(&self.focus);
        if self.changes.is_empty() {
            return;
        }
        self.reviewing = true;
        cx.notify();
    }

    fn discard(&mut self, _: &DiscardChanges, _: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.changes = Changes::default();
        self.reviewing = false;
        self.set_notice(None, cx);
    }

    fn apply(&mut self, _: &ApplyChanges, _: &mut Window, cx: &mut Context<Self>) {
        if self.applying.is_some() {
            return;
        }
        let statements = match self.statements(cx) {
            Ok(statements) if !statements.is_empty() => statements,
            Ok(_) => return,
            Err(reason) => return self.set_notice(Some(reason), cx),
        };
        let name = self.connection.clone();
        let task = self
            .store
            .update(cx, |s, cx| s.apply(&name, statements, cx));
        self.applying = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.applying = None;
                match result {
                    // Show the rows as they are now.
                    Ok(()) => this.run(this.connection.clone(), this.query.to_string(), cx),
                    Err(e) => this.set_notice(Some(e.into()), cx),
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn render_changes_bar(&self, theme: &Theme) -> Option<gpui::AnyElement> {
        if self.changes.is_empty() && self.notice.is_none() {
            return None;
        }
        let rows = self.changes.len();
        let summary = match rows {
            0 => String::new(),
            1 => "1 changed row".into(),
            n => format!("{n} changed rows"),
        };
        let store = self.store.clone();
        let name = self.connection.to_string();
        Some(
            div()
                .flex_none()
                .h(px(34.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(theme.line)
                .bg(theme.bg_sunken)
                .text_size(UI_FONT_SIZE)
                .font_family(crate::theme::UI_FONT)
                .when(!summary.is_empty(), |d| {
                    d.child(div().flex_none().text_color(theme.accent).child(summary))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.error)
                        .children(self.notice.clone()),
                )
                .when(self.locked_notice, |d| {
                    d.child(ui::button(
                        "results-unlock",
                        "Allow changes",
                        false,
                        theme,
                        move |_, window, cx| {
                            database::confirm_unlock(store.clone(), name.clone(), window, cx)
                        },
                    ))
                })
                .when(rows > 0 && !self.reviewing, |d| {
                    d.child(ui::button(
                        "results-discard",
                        "Discard",
                        false,
                        theme,
                        |_, window, cx| window.dispatch_action(Box::new(DiscardChanges), cx),
                    ))
                    .child(ui::button(
                        "results-review",
                        "Review",
                        true,
                        theme,
                        |_, window, cx| window.dispatch_action(Box::new(ReviewChanges), cx),
                    ))
                })
                .when(self.reviewing, |d| {
                    d.child(ui::button(
                        "results-back",
                        "Back",
                        false,
                        theme,
                        |_, window, cx| window.dispatch_action(Box::new(CancelEdit), cx),
                    ))
                    .child(ui::button(
                        "results-apply",
                        if self.applying.is_some() {
                            "Saving..."
                        } else {
                            "Apply in one transaction"
                        },
                        true,
                        theme,
                        |_, window, cx| window.dispatch_action(Box::new(ApplyChanges), cx),
                    ))
                })
                .into_any_element(),
        )
    }

    fn render_review(&mut self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let statements = self.statements(cx).unwrap_or_default();
        let before = self.review_notes();
        div()
            .id("results-review")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .children(statements.into_iter().zip(before).map(|(s, note)| {
                let color = if s.starts_with("DELETE") {
                    theme.error
                } else {
                    theme.fg
                };
                div()
                    .pb_1()
                    .child(div().text_color(color).child(format!("{s};")))
                    .children(
                        note.map(|n| div().text_color(theme.fg_subtle).child(format!("-- {n}"))),
                    )
            }))
            .into_any_element()
    }

    /// For each statement, in the same row order, what an update replaces:
    /// `status: 'pending' -> 'paid'`. Deletes need no note.
    fn review_notes(&self) -> Vec<Option<String>> {
        let Some(result) = self.result() else {
            return Vec::new();
        };
        let mut rows: std::collections::BTreeSet<usize> =
            self.changes.cells.keys().map(|(row, _)| *row).collect();
        rows.extend(&self.changes.deleted);
        let show = |v: Option<&Value>| match v {
            None | Some(Value::Null) => "NULL".to_string(),
            Some(v @ (Value::Int(_) | Value::Float(_) | Value::Number(_) | Value::Bool(_))) => {
                v.display()
            }
            Some(v) => format!("'{}'", v.display()),
        };
        rows.into_iter()
            .map(|row| {
                if self.changes.deleted.contains(&row) {
                    return None;
                }
                let parts: Vec<String> = self
                    .changes
                    .cells
                    .range((row, 0)..(row + 1, 0))
                    .map(|(&(_, col), new)| {
                        let old = result.rows.get(row).and_then(|r| r.get(col));
                        let new = new.as_ref().map(|t| Value::Text(t.clone()));
                        format!(
                            "{}: {} -> {}",
                            result.columns[col].name,
                            show(old),
                            show(new.as_ref())
                        )
                    })
                    .collect();
                Some(parts.join(", "))
            })
            .chain(self.changes.inserted.iter().map(|_| None))
            .collect()
    }

    fn render_status(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (text, color): (SharedString, _) = match &self.state {
            State::Empty => ("".into(), theme.fg_subtle),
            State::Running => ("Running...".into(), theme.fg_subtle),
            State::Failed(e) => (e.clone(), theme.error),
            State::Done(result) => {
                let ms = result.elapsed.as_secs_f64() * 1000.;
                let text = if result.columns.is_empty() {
                    match result.affected {
                        Some(1) => format!("1 row affected · {ms:.0} ms"),
                        Some(n) => format!("{n} rows affected · {ms:.0} ms"),
                        None => format!("Done · {ms:.0} ms"),
                    }
                } else if result.truncated {
                    format!("First {} rows · {ms:.0} ms", result.rows.len())
                } else if result.rows.len() == 1 {
                    format!("1 row · {ms:.0} ms")
                } else {
                    format!("{} rows · {ms:.0} ms", result.rows.len())
                };
                (text.into(), theme.fg_muted)
            }
        };
        div()
            .flex_none()
            .h(px(30.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(theme.line)
            .text_size(UI_FONT_SIZE)
            .when(!self.connection.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1p5()
                        .rounded(px(6.))
                        .bg(theme.bg_elev)
                        .text_color(theme.fg_muted)
                        .child(self.connection.clone()),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(crate::theme::CODE_FONT)
                    .text_color(theme.fg_subtle)
                    .child(self.query.replace('\n', " ")),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(480.))
                    .truncate()
                    .text_color(color)
                    .child(text),
            )
            .when(
                matches!(&self.state, State::Done(r) if !r.columns.is_empty()),
                |d| {
                    d.child(
                        div()
                            .id("results-add-row")
                            .flex_none()
                            .px_1p5()
                            .rounded(px(6.))
                            .text_color(theme.fg_subtle)
                            .hover(|d| d.bg(theme.line).text_color(theme.fg))
                            .child("Add row")
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.focus(&this.focus);
                                this.insert_row(&InsertRow, window, cx)
                            })),
                    )
                },
            )
    }

    fn render_header(&self, result: &QueryResult, theme: &Theme) -> impl IntoElement {
        div()
            .flex_none()
            .h(ROW_HEIGHT)
            .flex()
            .overflow_hidden()
            .border_b_1()
            .border_color(theme.line)
            .bg(theme.bg_sunken)
            .child(
                div()
                    .flex()
                    .ml(-self.pan)
                    .child(div().flex_none().w(self.number_width))
                    .children(result.columns.iter().zip(&self.widths).map(|(c, w)| {
                        div()
                            .flex_none()
                            .w(*w)
                            .h(ROW_HEIGHT)
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .overflow_hidden()
                            .border_r_1()
                            .border_color(theme.line)
                            .child(div().flex_none().text_color(theme.fg).child(c.name.clone()))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.fg_subtle)
                                    .child(c.type_name.clone()),
                            )
                    })),
            )
    }

    fn render_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let theme = cx.theme().clone();
        let Some(result) = self.result().cloned() else {
            return Vec::new();
        };
        range
            .filter_map(|ix| {
                if ix >= self.row_count() {
                    return None;
                }
                let added = self.new_row(ix).is_some();
                let cells = self
                    .widths
                    .iter()
                    .enumerate()
                    .take(result.columns.len())
                    .map(|(col, w)| {
                        let selected = self.selected == Some((ix, col));
                        let deleted = self.changes.deleted.contains(&ix);
                        let editor = self
                            .editing
                            .as_ref()
                            .filter(|e| e.row == ix && e.column == col)
                            .map(|e| e.editor.clone());
                        let shown = self.shown(ix, col);
                        let staged = !matches!(shown, Shown::Read(_));
                        let (text, value) = match shown {
                            Shown::Read(value) => (value.display(), value),
                            Shown::Staged(Some(text)) => (text, Value::Null),
                            Shown::Staged(None) => ("NULL".into(), Value::Null),
                            Shown::Default => ("DEFAULT".into(), Value::Null),
                        };
                        let default = text == "DEFAULT" && added;
                        let text: String = text
                            .chars()
                            .take(CELL_CHARS)
                            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                            .collect();
                        div()
                            .flex_none()
                            .w(*w)
                            .h(ROW_HEIGHT)
                            .px_2()
                            .flex()
                            .items_center()
                            .when(value.is_numeric(), |d| d.justify_end())
                            .overflow_hidden()
                            .border_r_1()
                            .border_color(theme.line)
                            .text_color(match value {
                                _ if deleted => theme.error,
                                _ if default => theme.fg_subtle,
                                _ if staged => theme.accent,
                                Value::Null => theme.fg_subtle,
                                Value::Int(_) | Value::Float(_) | Value::Number(_) => {
                                    theme.syntax.number
                                }
                                Value::Bool(_) => theme.syntax.keyword,
                                _ => theme.fg,
                            })
                            .when((staged || added) && !deleted, |d| d.bg(theme.accent_soft))
                            .when(deleted, |d| d.line_through())
                            .when(selected, |d| {
                                d.bg(theme.selection).border_1().border_color(theme.accent)
                            })
                            .map(|d| match editor {
                                Some(editor) => d.bg(theme.bg).child(div().flex_1().child(editor)),
                                None => d.child(div().truncate().child(text)),
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(
                                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                                        if this
                                            .editing
                                            .as_ref()
                                            .is_some_and(|e| e.row == ix && e.column == col)
                                        {
                                            return;
                                        }
                                        window.focus(&this.focus);
                                        this.select(ix, col, cx);
                                        if event.click_count == 2 {
                                            this.edit_cell(&EditCell, window, cx);
                                        }
                                    },
                                ),
                            )
                    });
                Some(
                    div()
                        .h(ROW_HEIGHT)
                        .flex()
                        .border_b_1()
                        .border_color(theme.line)
                        .child(
                            div()
                                .flex()
                                .ml(-self.pan)
                                .child(
                                    div()
                                        .flex_none()
                                        .w(self.number_width)
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .justify_end()
                                        .text_color(theme.fg_subtle)
                                        .child(if added {
                                            "+".to_string()
                                        } else {
                                            (ix + 1).to_string()
                                        }),
                                )
                                .children(cells),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}

/// Wide enough for the header and the first rows, within limits.
fn column_widths(result: &QueryResult, char_width: Pixels) -> Vec<Pixels> {
    result
        .columns
        .iter()
        .enumerate()
        .map(|(i, column)| {
            let header = column.name.chars().count() + column.type_name.chars().count() + 1;
            let widest = result
                .rows
                .iter()
                .take(200)
                .filter_map(|r| r.get(i))
                .map(|v| v.display().chars().count().min(60))
                .max()
                .unwrap_or(0)
                .max(header);
            px(f32::from(char_width * widest as f32 + px(20.)).clamp(MIN_COLUMN, MAX_COLUMN))
        })
        .collect()
}

impl Focusable for ResultsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ResultsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let settings = Settings::get(cx).clone();
        let changes_bar = self.render_changes_bar(&theme);
        let body = match &self.state {
            State::Done(_) if self.reviewing => self.render_review(&theme, cx),
            State::Done(result) if !result.columns.is_empty() => {
                let result = result.clone();
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.render_header(&result, &theme))
                    .child(
                        uniform_list(
                            "result-rows",
                            self.row_count(),
                            cx.processor(|this, range, _, cx| this.render_rows(range, cx)),
                        )
                        .track_scroll(self.scroll.clone())
                        .flex_1(),
                    )
                    .into_any_element()
            }
            State::Empty => div()
                .p_3()
                .text_color(theme.fg_subtle)
                .child("Run a query to see results here.")
                .into_any_element(),
            _ => div().into_any_element(),
        };
        let view = cx.entity();
        let mut context = gpui::KeyContext::new_with_defaults();
        context.add("ResultsGrid");
        if self.editing.is_some() {
            context.add("editing");
        }
        div()
            .id("results")
            .key_context(context)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::edit_cell))
            .on_action(cx.listener(Self::edit_inline))
            .on_action(cx.listener(Self::insert_row))
            .on_action(cx.listener(Self::duplicate_row))
            .on_action(cx.listener(Self::cancel_edit))
            .on_action(cx.listener(Self::set_null))
            .on_action(cx.listener(Self::delete_row))
            .on_action(cx.listener(Self::review))
            .on_action(cx.listener(Self::discard))
            .on_action(cx.listener(Self::apply))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::copy_cell))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .font_family(settings.buffer_font_family.clone())
            .text_size(settings.buffer_font_size() - px(1.))
            .child(self.render_status(&theme, cx))
            .children(changes_bar)
            .child(body)
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, _, cx| {
                        view.update(cx, |view, _| view.viewport = bounds.size.width);
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::Column;

    #[test]
    fn widths_follow_content_within_limits() {
        let result = QueryResult {
            columns: vec![
                Column {
                    name: "id".into(),
                    type_name: "int4".into(),
                },
                Column {
                    name: "bio".into(),
                    type_name: "text".into(),
                },
            ],
            rows: vec![vec![Value::Int(1), Value::Text("x".repeat(500))]],
            ..Default::default()
        };
        let widths = column_widths(&result, px(8.));
        assert_eq!(widths[0], px(MIN_COLUMN).max(px(8. * 7. + 20.)));
        assert_eq!(widths[1], px(MAX_COLUMN));
    }
}
