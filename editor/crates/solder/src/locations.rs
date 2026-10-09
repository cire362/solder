//! Pickers for language-server results: a list of locations (references,
//! several definitions) and the rename prompt.

use std::path::PathBuf;

use gpui::{
    AnyElement, Context, DismissEvent, Entity, SharedString, Task, WeakEntity, Window, div,
    prelude::*, px,
};

use crate::{
    editor::Editor,
    editor_lsp::LspLocation,
    fuzzy,
    lsp_store::ServerAction,
    picker::{Picker, PickerDelegate, highlighted_text},
    theme::{ActiveTheme, UI_FONT_SIZE},
    workspace::{Jump, Workspace},
};

struct Row {
    location: LspLocation,
    /// `src/main.rs:12`
    label: String,
    preview: SharedString,
}

pub struct LocationPicker {
    workspace: WeakEntity<Workspace>,
    title: SharedString,
    rows: Vec<Row>,
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
}

impl LocationPicker {
    /// `line_text` returns the text of a 0-based line of a file, from an open
    /// document when there is one.
    pub fn new(
        workspace: WeakEntity<Workspace>,
        title: SharedString,
        root: &std::path::Path,
        locations: Vec<LspLocation>,
        mut line_text: impl FnMut(&PathBuf, usize) -> Option<String>,
    ) -> Self {
        let rows = locations
            .into_iter()
            .map(|location| {
                let rel = location
                    .path
                    .strip_prefix(root)
                    .unwrap_or(&location.path)
                    .display()
                    .to_string();
                let line = location.range.start.line as usize;
                let preview = line_text(&location.path, line)
                    .map(|t| t.trim().chars().take(160).collect::<String>())
                    .unwrap_or_default();
                Row {
                    label: format!("{rel}:{}", line + 1),
                    preview: preview.into(),
                    location,
                }
            })
            .collect();
        Self {
            workspace,
            title,
            rows,
            matches: Vec::new(),
            selected: 0,
        }
    }
}

impl PickerDelegate for LocationPicker {
    fn placeholder(&self) -> SharedString {
        format!("{} ({})", self.title, self.rows.len()).into()
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
        self.matches = if query.is_empty() {
            (0..self.rows.len()).map(|i| (i, Vec::new())).collect()
        } else {
            fuzzy::fuzzy_match(self.rows.iter().map(|r| r.label.as_str()), query, 500, true)
                .into_iter()
                .map(|m| {
                    (
                        m.index,
                        fuzzy::positions(&self.rows[m.index].label, query, true),
                    )
                })
                .collect()
        };
        self.selected = 0;
        Task::ready(())
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((ix, _)) = self.matches.get(self.selected) else {
            return;
        };
        let location = self.rows[*ix].location.clone();
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |w, cx| {
                let jump = Jump::Lsp {
                    range: location.range,
                    encoding: location.encoding,
                };
                w.open_path(location.path, Some(jump), window, cx)
            })
            .ok();
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (row_ix, positions) = &self.matches[ix];
        let row = &self.rows[*row_ix];
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .text_size(UI_FONT_SIZE)
            .child(
                div()
                    .flex_none()
                    .text_color(theme.fg)
                    .child(highlighted_text(
                        &row.label,
                        positions,
                        theme.fg,
                        theme.accent,
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::theme::text(11.5))
                    .text_color(theme.fg_subtle)
                    .child(row.preview.clone()),
            )
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        format!("No {}", self.title.to_lowercase()).into()
    }

    fn width(&self) -> gpui::Pixels {
        px(720.)
    }
}

/// F2: type the new name, Enter to apply across the project.
pub struct RenamePrompt {
    editor: Entity<Editor>,
    current: String,
    new_name: String,
}

impl RenamePrompt {
    pub fn new(editor: Entity<Editor>, current: String) -> Self {
        Self {
            editor,
            new_name: current.clone(),
            current,
        }
    }
}

impl PickerDelegate for RenamePrompt {
    fn placeholder(&self) -> SharedString {
        format!("Rename {}", self.current).into()
    }

    fn match_count(&self) -> usize {
        usize::from(!self.new_name.is_empty() && self.new_name != self.current)
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
        self.new_name = query.trim().to_string();
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        if self.match_count() == 0 {
            return;
        }
        let name = self.new_name.clone();
        self.editor.update(cx, |e, cx| e.perform_rename(name, cx));
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
            .text_color(cx.theme().fg)
            .child(format!(
                "Rename \u{201c}{}\u{201d} to \u{201c}{}\u{201d}",
                self.current, self.new_name
            ))
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        "Type a new name".into()
    }

    fn width(&self) -> gpui::Pixels {
        px(460.)
    }
}

/// `cmd-.`: pick one of the code actions the file's servers offer.
pub struct CodeActionPicker {
    editor: Entity<Editor>,
    actions: Vec<ServerAction>,
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
}

impl CodeActionPicker {
    pub fn new(editor: Entity<Editor>, actions: Vec<ServerAction>) -> Self {
        Self {
            editor,
            actions,
            matches: Vec::new(),
            selected: 0,
        }
    }

    fn title(action: &ServerAction) -> &str {
        match &action.action {
            lsp::types::CodeActionOrCommand::Command(c) => &c.title,
            lsp::types::CodeActionOrCommand::CodeAction(a) => &a.title,
        }
    }
}

impl PickerDelegate for CodeActionPicker {
    fn placeholder(&self) -> SharedString {
        "Code actions".into()
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
        self.matches = if query.is_empty() {
            (0..self.actions.len()).map(|i| (i, Vec::new())).collect()
        } else {
            fuzzy::fuzzy_match(self.actions.iter().map(Self::title), query, 100, false)
                .into_iter()
                .map(|m| {
                    let title = Self::title(&self.actions[m.index]);
                    (m.index, fuzzy::positions(title, query, false))
                })
                .collect()
        };
        self.selected = 0;
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((ix, _)) = self.matches.get(self.selected) else {
            return;
        };
        let action = self.actions[*ix].clone();
        self.editor
            .update(cx, |e, cx| e.apply_code_action(action, cx));
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
        let (action_ix, positions) = &self.matches[ix];
        div()
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .child(highlighted_text(
                Self::title(&self.actions[*action_ix]),
                positions,
                theme.fg,
                theme.accent,
            ))
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        "No code actions here".into()
    }
}
