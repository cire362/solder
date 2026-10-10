//! What an extension asks the user: one of a list, or a line of text. Both
//! are the editor's own picker, so they look and answer to the keys like
//! the command palette. Closed without an answer, the extension hears that
//! nothing was chosen.

use gpui::{AnyElement, Context, DismissEvent, SharedString, Task, Window, div, prelude::*, px};

use crate::{
    extension_api::{OfferedTask, PickRow, Reply},
    extension_store::ExtensionStore,
    fuzzy,
    picker::{Picker, PickerDelegate, highlighted_text},
    theme::{ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
};

/// A list an extension shows to pick one of: a message with its answers,
/// or a quick pick. In one where several may be picked, Enter ticks a row
/// or takes the tick away, and the first row, which is none of the
/// extension's, answers with the ones that are ticked.
pub struct AskPick {
    title: String,
    rows: Vec<PickRow>,
    many: bool,
    /// The rows that match what was typed, with where. `None` is the row
    /// that answers.
    matches: Vec<(Option<usize>, Vec<u32>)>,
    selected: usize,
    reply: Option<Reply>,
}

impl AskPick {
    pub fn new(title: String, rows: Vec<PickRow>, many: bool, reply: Reply) -> Self {
        Self {
            title,
            rows,
            many,
            matches: Vec::new(),
            selected: 0,
            reply: Some(reply),
        }
    }

    #[cfg(test)]
    pub fn picked(&self) -> Vec<&str> {
        let picked = self.rows.iter().filter(|row| row.picked);
        picked.map(|row| row.label.as_str()).collect()
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
        let mut matches: Vec<(Option<usize>, Vec<u32>)> = if query.is_empty() {
            (0..self.rows.len())
                .map(|ix| (Some(ix), Vec::new()))
                .collect()
        } else {
            let labels = self.rows.iter().map(|row| row.label.as_str());
            fuzzy::fuzzy_match(labels, &query, 200, false)
                .into_iter()
                .map(|found| {
                    let label = &self.rows[found.index].label;
                    (Some(found.index), fuzzy::positions(label, &query, false))
                })
                .collect()
        };
        // The row that answers is there whatever was typed.
        if self.many {
            matches.insert(0, (None, Vec::new()));
        }
        self.matches = matches;
        // In a list of several the first of the extension's rows is the
        // one in hand: Enter there ticks, and does not answer by mistake.
        self.selected = usize::from(self.many && self.matches.len() > 1);
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((row, _)) = self.matches.get(self.selected).cloned() else {
            cx.emit(DismissEvent);
            return;
        };
        match (row, self.many) {
            // One of several: ticked, or the tick taken away. The list stays.
            (Some(ix), true) => {
                self.rows[ix].picked = !self.rows[ix].picked;
                cx.notify();
                return;
            }
            (Some(ix), false) => {
                if let Some(reply) = self.reply.take() {
                    reply.send(Ok(serde_json::json!(ix)));
                }
            }
            (None, _) => {
                let picked: Vec<usize> = (0..self.rows.len())
                    .filter(|ix| self.rows[*ix].picked)
                    .collect();
                if let Some(reply) = self.reply.take() {
                    reply.send(Ok(serde_json::json!(picked)));
                }
            }
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
        let Some(row) = row.map(|row| &self.rows[row]) else {
            let picked = self.rows.iter().filter(|row| row.picked).count();
            let text = match picked {
                0 => "Answer with none picked".to_string(),
                1 => "Answer with the 1 picked".to_string(),
                n => format!("Answer with the {n} picked"),
            };
            return div()
                .text_size(UI_FONT_SIZE)
                .text_color(theme.accent)
                .child(text)
                .into_any_element();
        };
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
            .when(self.many, |d| {
                // The tick, or the room it takes, so that labels line up.
                d.child(
                    div()
                        .w(px(14.))
                        .flex_none()
                        .text_color(theme.accent)
                        .child(if row.picked { "\u{2713}" } else { "" }),
                )
            })
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
            .debug_selector(|| "extension-input-guidance".into())
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

/// The tasks of extensions, to run one. The extensions are asked when the
/// list opens, and some are started for it: until they answered, it has
/// nothing to show.
pub struct TaskPick {
    tasks: Vec<OfferedTask>,
    loading: bool,
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
}

impl Default for TaskPick {
    fn default() -> Self {
        Self {
            tasks: Vec::new(),
            loading: true,
            matches: Vec::new(),
            selected: 0,
        }
    }
}

impl TaskPick {
    /// Whether the extensions have yet to say what tasks they have.
    #[cfg(test)]
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Fills the list once the extensions have said what tasks they have.
    pub fn load(
        listing: Task<Vec<OfferedTask>>,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        cx.spawn_in(window, async move |picker, cx| {
            let tasks = listing.await;
            picker
                .update_in(cx, |picker, window, cx| {
                    picker.delegate.tasks = tasks;
                    picker.delegate.loading = false;
                    picker.refresh(window, cx);
                })
                .ok();
        })
        .detach();
    }

    /// A task as its row names it: what it is a task of, then its name.
    fn title(task: &OfferedTask) -> String {
        match task.source.is_empty() {
            true => task.name.clone(),
            false => format!("{}: {}", task.source, task.name),
        }
    }
}

impl PickerDelegate for TaskPick {
    fn placeholder(&self) -> SharedString {
        "Run a task of an extension".into()
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
        let query = query.trim();
        let titles: Vec<String> = self.tasks.iter().map(Self::title).collect();
        self.matches = if query.is_empty() {
            (0..titles.len()).map(|ix| (ix, Vec::new())).collect()
        } else {
            fuzzy::fuzzy_match(titles.iter().map(String::as_str), query, 200, false)
                .into_iter()
                .map(|m| (m.index, fuzzy::positions(&titles[m.index], query, false)))
                .collect()
        };
        self.selected = 0;
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((ix, _)) = self.matches.get(self.selected) else {
            return;
        };
        let task = self.tasks[*ix].clone();
        cx.emit(DismissEvent);
        let Some(store) = ExtensionStore::try_global(cx) else {
            return;
        };
        let running = store.update(cx, |store, cx| store.run_task(&task, cx));
        // That it could not be run is said in the status bar.
        cx.spawn(async move |_, cx| {
            if let Err(error) = running.await {
                store
                    .update(cx, |store, cx| {
                        store.report(format!("{}: {error}", task.name), cx)
                    })
                    .ok();
            }
        })
        .detach();
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let (ix, positions) = &self.matches[ix];
        let theme = cx.theme();
        div()
            .text_size(UI_FONT_SIZE)
            .child(highlighted_text(
                &Self::title(&self.tasks[*ix]),
                positions,
                theme.fg,
                theme.accent,
            ))
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        if self.loading {
            "Asking the extensions...".into()
        } else if self.tasks.is_empty() {
            "No extension has a task to run".into()
        } else {
            "No task of that name".into()
        }
    }
}
