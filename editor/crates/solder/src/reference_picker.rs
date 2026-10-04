//! Picks the value of a foreign key cell from the rows it can point at:
//! their key and, when the table has one, a text column to recognize them
//! by. What is typed can also be used as is.

use gpui::{
    AnyElement, Context, DismissEvent, SharedString, Task, WeakEntity, Window, div, prelude::*, px,
};

use crate::{
    database::DatabaseStore,
    fuzzy,
    picker::{Picker, PickerDelegate},
    results::ResultsView,
    theme::{ActiveTheme, UI_FONT_SIZE},
};

pub struct ReferencePicker {
    results: WeakEntity<ResultsView>,
    row: usize,
    column: usize,
    title: SharedString,
    /// `(key, label)` of each row, once loaded.
    rows: Option<Result<Vec<(String, String)>, SharedString>>,
    matches: Vec<usize>,
    selected: usize,
    typed: String,
    load: Option<(gpui::Entity<DatabaseStore>, SharedString, String)>,
    _loading: Option<Task<()>>,
}

impl ReferencePicker {
    pub fn new(
        results: WeakEntity<ResultsView>,
        store: gpui::Entity<DatabaseStore>,
        row: usize,
        column: usize,
        connection: SharedString,
        query: String,
        title: SharedString,
    ) -> Self {
        Self {
            results,
            row,
            column,
            title,
            rows: None,
            matches: Vec::new(),
            selected: 0,
            typed: String::new(),
            load: Some((store, connection, query)),
            _loading: None,
        }
    }

    fn filter(&mut self) {
        let Some(Ok(rows)) = &self.rows else {
            self.matches.clear();
            return;
        };
        let keys: Vec<String> = rows.iter().map(|(k, l)| format!("{k} {l}")).collect();
        self.matches = if self.typed.is_empty() {
            (0..rows.len()).collect()
        } else {
            fuzzy::fuzzy_match(keys.iter().map(String::as_str), &self.typed, 200, false)
                .into_iter()
                .map(|m| m.index)
                .collect()
        };
        self.selected = 0;
    }

    /// The typed text is offered as a value when it matches no row.
    fn offers_typed(&self) -> bool {
        !self.typed.is_empty() && self.matches.is_empty()
    }
}

impl PickerDelegate for ReferencePicker {
    fn placeholder(&self) -> SharedString {
        format!("Pick from {}", self.title).into()
    }

    fn match_count(&self) -> usize {
        if self.offers_typed() {
            1
        } else {
            self.matches.len()
        }
    }

    fn selected_index(&self) -> usize {
        self.selected
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Context<Picker<Self>>) {
        self.selected = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.typed = query.trim().to_string();
        // Load once; the task lives here so typing does not cancel it.
        if let Some((store, connection, query)) = self.load.take() {
            let task = store.update(cx, |s, cx| s.run(&connection, query, cx));
            self._loading = Some(cx.spawn_in(window, async move |picker, cx| {
                let result = task.await;
                picker
                    .update(cx, |picker, cx| {
                        let delegate = &mut picker.delegate;
                        delegate.rows = Some(
                            result
                                .map(|r| {
                                    r.rows
                                        .into_iter()
                                        .map(|row| {
                                            let key = row.first().map(db::Value::display);
                                            let label = row.get(1).map(db::Value::display);
                                            (key.unwrap_or_default(), label.unwrap_or_default())
                                        })
                                        .collect()
                                })
                                .map_err(Into::into),
                        );
                        delegate.filter();
                        picker.matches_updated(cx);
                    })
                    .ok();
            }));
        }
        self.filter();
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let value = if self.offers_typed() {
            self.typed.clone()
        } else {
            match (&self.rows, self.matches.get(self.selected)) {
                (Some(Ok(rows)), Some(&ix)) => rows[ix].0.clone(),
                _ => return,
            }
        };
        let (row, column) = (self.row, self.column);
        self.results
            .update(cx, |results, cx| {
                results.stage(row, column, Some(value), cx)
            })
            .ok();
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let theme = cx.theme();
        if self.offers_typed() {
            return div()
                .text_size(UI_FONT_SIZE)
                .text_color(theme.fg)
                .child(format!("Use \u{201c}{}\u{201d}", self.typed))
                .into_any_element();
        }
        let Some(Ok(rows)) = &self.rows else {
            return div().into_any_element();
        };
        let (key, label) = &rows[self.matches[ix]];
        div()
            .flex()
            .gap_2()
            .text_size(UI_FONT_SIZE)
            .child(
                div()
                    .flex_none()
                    .font_family(crate::theme::CODE_FONT)
                    .text_color(theme.syntax.number)
                    .child(key.clone()),
            )
            .child(div().truncate().text_color(theme.fg).child(label.clone()))
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        match &self.rows {
            None => "Loading...".into(),
            Some(Err(e)) => e.clone(),
            Some(Ok(_)) => "No rows. Type a value to use it.".into(),
        }
    }

    fn width(&self) -> gpui::Pixels {
        px(460.)
    }
}
