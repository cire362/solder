//! The prompts of context servers: texts a server writes for the user to
//! send as a task. One is chosen from a list of all the running servers
//! have, told what it has to be told, and put in the agent's field.

use gpui::{
    AnyElement, Context, DismissEvent, SharedString, Task, WeakEntity, Window, div, prelude::*, px,
};

use crate::{
    fuzzy,
    mcp_store::{McpStore, ServerPrompt},
    picker::{Picker, PickerDelegate, highlighted_text},
    theme::{ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
    workspace::Workspace,
};

/// The list of prompts. Servers start with it, as they do with a task:
/// until they have, it has nothing to show.
pub struct Prompts {
    workspace: WeakEntity<Workspace>,
    prompts: Vec<ServerPrompt>,
    starting: bool,
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
}

impl Prompts {
    pub fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self {
            workspace,
            prompts: Vec::new(),
            starting: true,
            matches: Vec::new(),
            selected: 0,
        }
    }

    /// Fills the list once the servers have started or failed.
    pub fn load(starting: Task<()>, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        cx.spawn_in(window, async move |picker, cx| {
            starting.await;
            picker
                .update_in(cx, |picker, window, cx| {
                    picker.delegate.prompts = McpStore::global(cx).read(cx).prompts();
                    picker.delegate.starting = false;
                    picker.refresh(window, cx);
                })
                .ok();
        })
        .detach();
    }

    /// A prompt as its row names it: the server it is of, then its own
    /// name.
    fn title(prompt: &ServerPrompt) -> String {
        format!("{}: {}", prompt.server_name, prompt.prompt.name)
    }
}

impl PickerDelegate for Prompts {
    fn placeholder(&self) -> SharedString {
        "Use a prompt of a context server".into()
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
        let titles: Vec<String> = self.prompts.iter().map(Self::title).collect();
        self.matches = if query.is_empty() {
            (0..titles.len()).map(|ix| (ix, Vec::new())).collect()
        } else {
            fuzzy::fuzzy_match(titles.iter().map(String::as_str), query, 100, false)
                .into_iter()
                .map(|m| (m.index, fuzzy::positions(&titles[m.index], query, false)))
                .collect()
        };
        self.selected = 0;
        Task::ready(())
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((ix, _)) = self.matches.get(self.selected) else {
            return;
        };
        let (chosen, workspace) = (self.prompts[*ix].clone(), self.workspace.clone());
        cx.emit(DismissEvent);
        // After this list has closed: the next thing may be another one.
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.fill_prompt(chosen, Vec::new(), 0, window, cx)
                })
                .ok();
        });
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let (ix, positions) = &self.matches[ix];
        let prompt = &self.prompts[*ix];
        let theme = cx.theme();
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .child(highlighted_text(
                &Self::title(prompt),
                positions,
                theme.fg,
                theme.accent,
            ))
            .child(
                div()
                    .truncate()
                    .text_size(UI_FONT_SMALL)
                    .text_color(theme.fg_subtle)
                    .child(prompt.prompt.description.clone()),
            )
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        if self.starting {
            "Starting the context servers...".into()
        } else if self.prompts.is_empty() {
            "No context server that runs has a prompt".into()
        } else {
            "No prompt of that name".into()
        }
    }

    fn width(&self) -> gpui::Pixels {
        px(520.)
    }
}

/// One thing a prompt has to be told, asked for in a line of its own.
pub struct Told {
    workspace: WeakEntity<Workspace>,
    chosen: ServerPrompt,
    told: Vec<(String, String)>,
    /// Which of the prompt's arguments this line is for.
    asked: usize,
    value: String,
}

impl Told {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        chosen: ServerPrompt,
        told: Vec<(String, String)>,
        asked: usize,
    ) -> Self {
        Self {
            workspace,
            chosen,
            told,
            asked,
            value: String::new(),
        }
    }

    fn argument(&self) -> &ai::mcp::Argument {
        &self.chosen.prompt.arguments[self.asked]
    }
}

impl PickerDelegate for Told {
    fn placeholder(&self) -> SharedString {
        let argument = self.argument();
        let mut text = argument.name.clone();
        if !argument.description.is_empty() {
            text.push_str(": ");
            text.push_str(&argument.description);
        }
        if !argument.required {
            text.push_str(" (may be left out)");
        }
        text.into()
    }

    // What must be told has to be typed; the rest may be left empty.
    fn match_count(&self) -> usize {
        usize::from(!self.value.is_empty() || !self.argument().required)
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
        self.value = query.trim().to_string();
        Task::ready(())
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if self.match_count() == 0 {
            return;
        }
        let mut told = std::mem::take(&mut self.told);
        if !self.value.is_empty() {
            told.push((self.argument().name.clone(), self.value.clone()));
        }
        let (chosen, workspace, next) =
            (self.chosen.clone(), self.workspace.clone(), self.asked + 1);
        cx.emit(DismissEvent);
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.fill_prompt(chosen, told, next, window, cx)
                })
                .ok();
        });
    }

    fn render_match(
        &self,
        _: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let name = &self.argument().name;
        div()
            .text_size(UI_FONT_SIZE)
            .text_color(cx.theme().fg)
            .child(if self.value.is_empty() {
                format!("Leave {name} out")
            } else {
                format!("{name} is \u{201c}{}\u{201d}", self.value)
            })
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        format!("Type {}", self.argument().name).into()
    }

    fn width(&self) -> gpui::Pixels {
        px(520.)
    }
}

/// The text of a prompt as the agent's one line takes it: its paragraphs
/// one after the other.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
