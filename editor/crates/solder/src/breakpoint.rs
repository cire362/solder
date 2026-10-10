use std::path::PathBuf;

use gpui::{
    AnyElement, Context, DismissEvent, Entity, SharedString, Task, Window, div, prelude::*,
};

use crate::{
    debug::DebugStore,
    picker::{Picker, PickerDelegate},
    theme::{ActiveTheme, UI_FONT_SIZE},
};

#[derive(Clone, Copy)]
pub enum Field {
    Condition,
    Hits,
    Log,
}

pub struct Prompt {
    store: Entity<DebugStore>,
    path: PathBuf,
    line: u32,
    field: Field,
    query: String,
}

impl Prompt {
    pub fn new(store: Entity<DebugStore>, path: PathBuf, line: u32, field: Field) -> Self {
        Self {
            store,
            path,
            line,
            field,
            query: String::new(),
        }
    }
    pub fn value(&self, cx: &gpui::App) -> String {
        let options = self.store.read(cx).options(&self.path, self.line);
        match self.field {
            Field::Condition => options.condition,
            Field::Hits => options.hits,
            Field::Log => options.log,
        }
    }
}

impl PickerDelegate for Prompt {
    fn placeholder(&self) -> SharedString {
        match self.field {
            Field::Condition => "Breakpoint condition; empty clears it",
            Field::Hits => "Hit condition; syntax belongs to the debugger",
            Field::Log => "Log message; expressions go in {braces}",
        }
        .into()
    }
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
        self.query = query;
        Task::ready(())
    }
    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let mut options = self.store.read(cx).options(&self.path, self.line);
        let text = self.query.trim().to_string();
        match self.field {
            Field::Condition => options.condition = text,
            Field::Hits => options.hits = text,
            Field::Log => options.log = text,
        }
        self.store.update(cx, |s, cx| {
            s.set_options(&self.path, self.line, options, cx)
        });
        cx.emit(DismissEvent);
    }
    fn render_match(
        &self,
        _: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let label = if self.query.trim().is_empty() {
            "Clear this setting"
        } else {
            "Set this setting"
        };
        div()
            .text_size(UI_FONT_SIZE)
            .text_color(cx.theme().fg)
            .child(format!("{label} at line {}", self.line))
            .into_any_element()
    }
}
