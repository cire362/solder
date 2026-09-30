//! The Results tab in the bottom dock: the last query's rows in a grid.
//! Rows are virtualized; columns pan together with the header.

use std::{ops::Range, sync::Arc};

use db::{QueryResult, Value};
use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, KeyBinding, MouseButton, Pixels,
    ScrollStrategy, ScrollWheelEvent, SharedString, Task, UniformListScrollHandle, Window, actions,
    canvas, div, prelude::*, px, uniform_list,
};

use crate::{
    database::DatabaseStore,
    settings::Settings,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
};

actions!(
    results,
    [SelectUp, SelectDown, SelectLeft, SelectRight, CopyCell]
);

pub fn bind_keys(cx: &mut App) {
    let context = Some("ResultsGrid");
    cx.bind_keys([
        KeyBinding::new("up", SelectUp, context),
        KeyBinding::new("down", SelectDown, context),
        KeyBinding::new("left", SelectLeft, context),
        KeyBinding::new("right", SelectRight, context),
        KeyBinding::new("secondary-c", CopyCell, context),
    ]);
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
        }
    }

    /// Runs `query` on the named connection and shows what comes back.
    pub fn run(&mut self, connection: SharedString, query: String, cx: &mut Context<Self>) {
        self.connection = connection.clone();
        self.query = query.clone().into();
        self.state = State::Running;
        self.selected = None;
        self.pan = px(0.);
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
        if result.rows.is_empty() || result.columns.is_empty() {
            return;
        }
        let (row, column) = self.selected.unwrap_or((0, 0));
        let row = row.saturating_add_signed(rows).min(result.rows.len() - 1);
        let column = column
            .saturating_add_signed(columns)
            .min(result.columns.len() - 1);
        self.select(row, column, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, 0, cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, 0, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(0, -1, cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(0, 1, cx);
    }

    fn copy_cell(&mut self, _: &CopyCell, _: &mut Window, cx: &mut Context<Self>) {
        let value = self
            .result()
            .zip(self.selected)
            .and_then(|(result, (row, column))| {
                result
                    .rows
                    .get(row)
                    .and_then(|r| r.get(column))
                    .map(|v| match v {
                        Value::Null => String::new(),
                        other => other.display(),
                    })
            });
        if let Some(value) = value {
            cx.write_to_clipboard(ClipboardItem::new_string(value));
        }
    }

    fn render_status(&self, theme: &Theme) -> impl IntoElement {
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
                let row = result.rows.get(ix)?;
                let cells = row
                    .iter()
                    .zip(&self.widths)
                    .enumerate()
                    .map(|(col, (value, w))| {
                        let selected = self.selected == Some((ix, col));
                        let text: String = value
                            .display()
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
                                Value::Null => theme.fg_subtle,
                                Value::Int(_) | Value::Float(_) | Value::Number(_) => {
                                    theme.syntax.number
                                }
                                Value::Bool(_) => theme.syntax.keyword,
                                _ => theme.fg,
                            })
                            .when(selected, |d| {
                                d.bg(theme.selection).border_1().border_color(theme.accent)
                            })
                            .child(div().truncate().child(text))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    window.focus(&this.focus);
                                    this.select(ix, col, cx);
                                }),
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
                                        .child((ix + 1).to_string()),
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
        let settings = Settings::get(cx);
        let body = match &self.state {
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
                            result.rows.len(),
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
        div()
            .id("results")
            .key_context("ResultsGrid")
            .track_focus(&self.focus)
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
            .child(self.render_status(&theme))
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
