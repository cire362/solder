//! What an extension asks the user: one of a list, or a line of text. Both
//! are the editor's own picker, so they look and answer to the keys like
//! the command palette. Closed without an answer, the extension hears that
//! nothing was chosen.

use gpui::{AnyElement, Context, DismissEvent, SharedString, Task, Window, div, prelude::*, px};

use crate::{
    extension_api::{PickRow, Reply},
    fuzzy,
    picker::{Picker, PickerDelegate, highlighted_text},
    theme::{ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
};

/// A list an extension shows to pick one of: a message with its answers,
/// or a quick pick.
pub struct AskPick {
    title: String,
    rows: Vec<PickRow>,
    /// The rows that match what was typed, with where.
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
    reply: Option<Reply>,
}

impl AskPick {
    pub fn new(title: String, rows: Vec<PickRow>, reply: Reply) -> Self {
        Self {
            title,
            rows,
            matches: Vec::new(),
            selected: 0,
            reply: Some(reply),
        }
    }
}

impl PickerDelegate for AskPick {
    fn placeholder(&self) -> SharedString {
        self.title.clone().into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
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
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        // With nothing typed, in the order the extension gave: it put the
        // likely one first.
        self.matches = if query.is_empty() {
            (0..self.rows.len()).map(|ix| (ix, Vec::new())).collect()
        } else {
            let labels = self.rows.iter().map(|row| row.label.as_str());
            fuzzy::fuzzy_match(labels, &query, 200, false)
                .into_iter()
                .map(|found| {
                    let label = &self.rows[found.index].label;
                    (found.index, fuzzy::positions(label, &query, false))
                })
                .collect()
        };
        self.selected = 0;
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        if let (Some((ix, _)), Some(reply)) = (self.matches.get(self.selected), self.reply.take()) {
            reply.send(Ok(serde_json::json!(ix)));
        }
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
        let (row, positions) = &self.matches[ix];
        let row = &self.rows[*row];
        let more = [&row.description, &row.detail]
            .into_iter()
            .filter(|text| !text.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("  ");
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(UI_FONT_SIZE)
            .child(highlighted_text(
                &row.label,
                positions,
                theme.fg,
                theme.accent,
            ))
            .when(!more.is_empty(), |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(UI_FONT_SMALL)
                        .text_color(theme.fg_subtle)
                        .child(more),
                )
            })
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        "Nothing matches".into()
    }
}

/// A line an extension asks the user to type.
pub struct AskInput {
    title: String,
    typed: String,
    reply: Option<Reply>,
}

impl AskInput {
    pub fn new(title: String, reply: Reply) -> Self {
        Self {
            title,
            typed: String::new(),
            reply: Some(reply),
        }
    }
}

impl PickerDelegate for AskInput {
    fn placeholder(&self) -> SharedString {
        self.title.clone().into()
    }

    // One row, which says what Enter does with what was typed.
    fn match_count(&self) -> usize {
        1
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
        self.typed = query;
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        if let Some(reply) = self.reply.take() {
            reply.send(Ok(serde_json::json!(self.typed)));
        }
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        _: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        div()
            .text_size(UI_FONT_SIZE)
            .text_color(cx.theme().fg_muted)
            .child(format!(
                "{}  (Enter to answer, Escape to cancel)",
                self.title
            ))
            .into_any_element()
    }

    fn width(&self) -> gpui::Pixels {
        px(520.)
    }
}
