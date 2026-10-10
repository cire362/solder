//! A notebook: a file an extension reads as a list of cells, some of them
//! code it runs.
//!
//! The extension does what is its own: it reads the file into cells and
//! writes them back, and it runs the code (`host/notebooks.js`). What is
//! on screen is here: a tab with the cells one under another, each an
//! editor as tall as its text, and under a cell what its last run put
//! out. Words and pictures are drawn. An output that is a page of its own
//! (HTML, a widget) is named and not drawn.

use crate::{
    document::{Document, DocumentEvent},
    editor::{Editor, Reveal},
    extension_store::ExtensionStore,
    settings::Settings,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE, UI_FONT_SMALL},
};
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Div, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, Image, ImageFormat, InteractiveElement, IntoElement, KeyBinding,
    ListAlignment, ListState, ObjectFit, ParentElement, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, StyledImage, Subscription, Task, Window, actions, div, img,
    list, prelude::FluentBuilder, px,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

actions!(
    notebook,
    [
        RunCell,
        RunCellAndNext,
        RunAll,
        Interrupt,
        AddCode,
        AddText,
        DeleteCell,
        MoveCellUp,
        MoveCellDown
    ]
);

pub fn bind_keys(cx: &mut App) {
    // In a cell's editor, where these keys mean other things in a file.
    let cell = Some("Notebook > Editor");
    cx.bind_keys([
        KeyBinding::new("secondary-enter", RunCell, cell),
        KeyBinding::new("shift-enter", RunCellAndNext, cell),
    ]);
}

/// How many lines of what a run wrote are drawn, and how many bytes: an
/// output is read under its cell, and a log of a million lines is not.
const OUTPUT_LINES: usize = 200;
const OUTPUT_BYTES: usize = 64 * 1024;
/// How far past the edge the row of the cursor is brought.
const REVEAL_MARGIN: f32 = 24.;

/// One thing a run put under its cell.
#[derive(Clone)]
pub(crate) enum Output {
    Words(SharedString),
    /// What went wrong, or what was written for errors.
    Error(SharedString),
    Picture(Arc<Image>),
    /// A form that is not drawn here: what it is called.
    Unknown(SharedString),
}

struct Cell {
    /// Its number, which stays its own wherever the cell is moved: the
    /// extension knows it by this.
    handle: u64,
    code: bool,
    language: String,
    editor: Entity<Editor>,
    outputs: Vec<Output>,
    /// Which run of the notebook its last one was.
    order: Option<u64>,
    running: bool,
    failed: bool,
    _edits: Subscription,
}

enum State {
    Reading,
    Ready,
    Failed(SharedString),
}

pub enum NotebookEvent {
    /// Its name in the tab is to be drawn again: it has changes, or none.
    Changed,
}

pub struct Notebook {
    pub path: PathBuf,
    /// The extension that reads it, and what it calls this kind.
    extension: String,
    kind: String,
    uri: String,
    state: State,
    cells: Vec<Cell>,
    next_handle: u64,
    /// The cell the cursor is in, or was in last.
    selected: usize,
    dirty: bool,
    generation: u64,
    ipynb: Option<crate::ipynb::File>,
    /// What runs its cells, as the extension calls it.
    runner: Option<SharedString>,
    /// The last thing that went wrong.
    note: Option<SharedString>,
    list: ListState,
    focus_handle: FocusHandle,
}

impl EventEmitter<NotebookEvent> for Notebook {}

impl Focusable for Notebook {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Notebook {
    /// Opens the file as a notebook of `kind`, read by `extension`.
    pub fn open(path: PathBuf, extension: String, kind: String, cx: &mut Context<Self>) -> Self {
        let uri = crate::extension_api::uri(&path);
        let me = cx.weak_entity();
        let reading = (!extension.is_empty())
            .then(|| ExtensionStore::try_global(cx))
            .flatten()
            .map(|store| {
                store.update(cx, |store, cx| {
                    store.notebook_opened(&extension, &uri, me);
                    let params = json!({ "type": kind, "uri": uri });
                    store.ask_host(&extension, "notebook.open", params, cx)
                })
            });
        let native_path = extension.is_empty().then(|| path.clone());
        cx.spawn(async move |this, cx| {
            if let Some(path) = native_path {
                let read = cx
                    .background_executor()
                    .spawn(async move { crate::ipynb::File::read(&path) })
                    .await;
                this.update(cx, |this, cx| match read {
                    Ok((file, read)) => {
                        this.ipynb = Some(file);
                        this.read(Ok(read), cx);
                    }
                    Err(why) => this.read(Err(why), cx),
                })
                .ok();
                return;
            }
            let read = match reading {
                Some(reading) => reading.await,
                None => Err("Extensions are not there to read it".into()),
            };
            this.update(cx, |this, cx| this.read(read, cx)).ok();
        })
        .detach();
        Self {
            path,
            extension,
            kind,
            uri,
            state: State::Reading,
            cells: Vec::new(),
            next_handle: 0,
            selected: 0,
            dirty: false,
            generation: 0,
            ipynb: None,
            runner: None,
            note: None,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn title(&self) -> String {
        let name = self.path.file_name();
        name.map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn reader(&self) -> (String, String) {
        (self.extension.clone(), self.kind.clone())
    }

    pub fn is(&self, path: &Path, extension: &str, kind: &str) -> bool {
        self.path == path && self.extension == extension && self.kind == kind
    }

    /// Puts the keys in the cell the cursor was in, or in the notebook
    /// while it has none.
    pub fn focus(&self, window: &mut Window, cx: &App) {
        match self.cells.get(self.selected) {
            Some(cell) => window.focus(&cell.editor.focus_handle(cx)),
            None => window.focus(&self.focus_handle),
        }
    }

    fn read(&mut self, read: Result<Value, String>, cx: &mut Context<Self>) {
        match read {
            Ok(read) => {
                self.runner = read["runner"]
                    .as_str()
                    .map(|label| label.to_string().into());
                for cell in read["cells"].as_array().into_iter().flatten() {
                    let handle = cell["handle"].as_u64().unwrap_or(self.next_handle);
                    self.next_handle = self.next_handle.max(handle + 1);
                    let (language, value) = (text(&cell["language"]), text(&cell["value"]));
                    let code = cell["code"] == true;
                    let mut made = self.cell(handle, code, language, &value, cx);
                    made.outputs = outputs(&cell["outputs"]);
                    made.order = cell["order"].as_u64();
                    self.cells.push(made);
                }
                self.list.reset(self.cells.len());
                self.state = State::Ready;
            }
            Err(why) => self.state = State::Failed(why.into()),
        }
        cx.notify();
    }

    fn cell(
        &mut self,
        handle: u64,
        code: bool,
        language: String,
        value: &str,
        cx: &mut Context<Self>,
    ) -> Cell {
        let document = cx.new(|cx| Document::cell(language_file(&language), value, cx));
        // The row of the cursor is brought into view by what the cells
        // are in, which is the list.
        let (list, me) = (self.list.clone(), cx.entity_id());
        let reveal: Reveal = Rc::new(move |top, bottom, cx| {
            let seen = list.viewport_bounds();
            let margin = px(REVEAL_MARGIN);
            if seen.size.height <= margin * 2. {
                return;
            }
            if top < seen.top() {
                list.scroll_by(top - seen.top() - margin);
            } else if bottom > seen.bottom() {
                list.scroll_by(bottom - seen.bottom() + margin);
            } else {
                return;
            }
            cx.notify(me);
        });
        let editor = cx.new(|cx| Editor::fitted(document.clone(), reveal, cx));
        let edits = cx.subscribe(&document, move |this, _, event, cx| {
            if matches!(event, DocumentEvent::Edited { .. }) {
                this.cell_edited(handle, cx);
            }
        });
        Cell {
            handle,
            code,
            language,
            editor,
            outputs: Vec::new(),
            order: None,
            running: false,
            failed: false,
            _edits: edits,
        }
    }

    fn tell(&self, method: &str, params: Value, cx: &App) {
        if let Some(store) = ExtensionStore::try_global(cx) {
            store.read(cx).tell_host(&self.extension, method, params);
        }
    }

    fn ask(
        &self,
        method: &'static str,
        params: Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<Value, String>> {
        match ExtensionStore::try_global(cx) {
            Some(store) => store.update(cx, |store, cx| {
                store.ask_host(&self.extension, method, params, cx)
            }),
            None => Task::ready(Err("Extensions are not there".into())),
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        if !self.dirty {
            self.dirty = true;
            cx.emit(NotebookEvent::Changed);
        }
        cx.notify();
    }

    fn cell_edited(&mut self, handle: u64, cx: &mut Context<Self>) {
        let Some(cell) = self.cells.iter().find(|cell| cell.handle == handle) else {
            return;
        };
        let value = cell.editor.read(cx).text(cx);
        let params = json!({ "uri": self.uri, "handle": handle, "value": value });
        self.tell("notebook.cell", params, cx);
        self.changed(cx);
    }

    /// Says which cells there are and in what order, after one was added,
    /// removed or moved. `new` is the one the extension does not know yet.
    fn cells_changed(&mut self, new: Option<u64>, cx: &mut Context<Self>) {
        let cells: Vec<Value> = self
            .cells
            .iter()
            .map(|cell| {
                if Some(cell.handle) != new {
                    return json!({ "handle": cell.handle });
                }
                let value = cell.editor.read(cx).text(cx);
                json!({ "handle": cell.handle, "code": cell.code, "language": cell.language, "value": value })
            })
            .collect();
        self.tell(
            "notebook.cells",
            json!({ "uri": self.uri, "cells": cells }),
            cx,
        );
        self.changed(cx);
    }

    /// Something its extension said of it.
    pub(crate) fn heard(&mut self, method: &str, params: &Value, cx: &mut Context<Self>) {
        let handle = params["handle"].as_u64();
        let cell = self
            .cells
            .iter_mut()
            .find(|cell| Some(cell.handle) == handle);
        match (method, cell) {
            ("notebook.outputs", Some(cell)) => cell.outputs = outputs(&params["outputs"]),
            ("notebook.run", Some(cell)) => {
                cell.running = params["running"] == true;
                if !cell.running {
                    cell.failed = params["failed"] == true;
                    cell.order = params["order"].as_u64().or(cell.order);
                }
            }
            ("notebook.runners", _) => {
                let mine = params["runners"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|runner| runner["type"] == self.kind.as_str());
                self.runner = mine.map(|runner| text(&runner["label"]).into());
            }
            ("notebook.saved", _) => {
                self.dirty = false;
                cx.emit(NotebookEvent::Changed);
            }
            _ => return,
        }
        cx.notify();
    }

    /// The code of its extension stopped: nothing runs, and nothing that
    /// was running will say it ended.
    pub(crate) fn host_gone(&mut self, cx: &mut Context<Self>) {
        self.runner = None;
        for cell in &mut self.cells {
            cell.running = false;
        }
        self.note = Some("Its extension stopped. Open the file again to run or save it.".into());
        cx.notify();
    }

    fn failed(&mut self, asked: Task<Result<Value, String>>, cx: &mut Context<Self>) {
        self.note = None;
        cx.spawn(async move |this, cx| {
            if let Err(why) = asked.await {
                this.update(cx, |this, cx| {
                    this.note = Some(why.into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn run(&mut self, handles: Vec<u64>, cx: &mut Context<Self>) {
        if handles.is_empty() {
            return;
        }
        let params = json!({ "uri": self.uri, "handles": handles });
        let asked = self.ask("notebook.execute", params, cx);
        self.failed(asked, cx);
    }

    fn run_at(&mut self, ix: usize, cx: &mut Context<Self>) {
        let cell = self.cells.get(ix).filter(|cell| cell.code);
        let handles = cell.map(|cell| cell.handle).into_iter().collect();
        self.run(handles, cx);
    }

    fn run_cell(&mut self, _: &RunCell, _: &mut Window, cx: &mut Context<Self>) {
        self.run_at(self.selected, cx);
    }

    /// Runs the cell and goes to the next, which is made if there is none.
    fn run_cell_and_next(
        &mut self,
        _: &RunCellAndNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_at(self.selected, cx);
        if self.selected + 1 == self.cells.len() {
            self.add(true, window, cx);
        } else {
            self.select(self.selected + 1, window, cx);
        }
    }

    fn run_all(&mut self, _: &RunAll, _: &mut Window, cx: &mut Context<Self>) {
        let code = self.cells.iter().filter(|cell| cell.code);
        let handles = code.map(|cell| cell.handle).collect();
        self.run(handles, cx);
    }

    fn interrupt(&mut self, _: &Interrupt, _: &mut Window, cx: &mut Context<Self>) {
        let asked = self.ask("notebook.interrupt", json!({ "uri": self.uri }), cx);
        self.failed(asked, cx);
    }

    pub(crate) fn select(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(cell) = self.cells.get(ix) {
            self.selected = ix;
            window.focus(&cell.editor.focus_handle(cx));
            self.list.scroll_to_reveal_item(ix);
            self.fronted(true, cx);
            cx.notify();
        }
    }

    /// A new cell under the one the cursor is in: of code in the language
    /// of the nearest code above it, or of text.
    fn add(&mut self, code: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.state, State::Ready) {
            return;
        }
        let at = (self.selected + 1).min(self.cells.len());
        let language = match code {
            true => {
                let above = self.cells[..at].iter().rev().find(|cell| cell.code);
                let any = above.or_else(|| self.cells.iter().find(|cell| cell.code));
                any.map_or_else(|| "plaintext".to_string(), |cell| cell.language.clone())
            }
            false => "markdown".to_string(),
        };
        let handle = self.next_handle;
        self.next_handle += 1;
        let cell = self.cell(handle, code, language, "", cx);
        self.cells.insert(at, cell);
        self.list.splice(at..at, 1);
        self.cells_changed(Some(handle), cx);
        self.select(at, window, cx);
    }

    fn add_code(&mut self, _: &AddCode, window: &mut Window, cx: &mut Context<Self>) {
        self.add(true, window, cx);
    }

    fn add_text(&mut self, _: &AddText, window: &mut Window, cx: &mut Context<Self>) {
        self.add(false, window, cx);
    }

    fn delete_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.cells.len() {
            return;
        }
        self.cells.remove(ix);
        self.list.splice(ix..ix + 1, 0);
        self.cells_changed(None, cx);
        self.selected = self.selected.min(self.cells.len().saturating_sub(1));
        match self.cells.is_empty() {
            true => window.focus(&self.focus_handle),
            false => self.select(self.selected, window, cx),
        }
    }

    fn delete_cell(&mut self, _: &DeleteCell, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_at(self.selected, window, cx);
    }

    /// Moves a cell one place up or down.
    fn move_at(&mut self, ix: usize, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        let to = match down {
            true => ix + 1,
            false => ix.wrapping_sub(1),
        };
        if ix >= self.cells.len() || to >= self.cells.len() {
            return;
        }
        self.cells.swap(ix, to);
        let first = ix.min(to);
        self.list.splice(first..first + 2, 2);
        self.cells_changed(None, cx);
        self.select(to, window, cx);
    }

    fn move_cell_up(&mut self, _: &MoveCellUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_at(self.selected, false, window, cx);
    }

    fn move_cell_down(&mut self, _: &MoveCellDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_at(self.selected, true, window, cx);
    }

    /// Has the extension write the file from the cells as they are.
    /// Resolves to whether it was written.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        let generation = self.generation;
        let asked = match self.ipynb.clone() {
            Some(file) => {
                let path = self.path.clone();
                let cells = self
                    .cells
                    .iter()
                    .map(|cell| (cell.handle, cell.code, cell.editor.read(cx).text(cx)))
                    .collect::<Vec<_>>();
                cx.background_executor()
                    .spawn(async move { file.save(&path, &cells).map(|_| Value::Bool(true)) })
            }
            None => self.ask("notebook.save", json!({ "uri": self.uri }), cx),
        };
        self.note = None;
        cx.spawn(async move |this, cx| {
            let saved = asked.await;
            let done = saved.is_ok();
            this.update(cx, |this, cx| {
                match saved {
                    Ok(_) if this.generation == generation => this.dirty = false,
                    Ok(_) => {}
                    Err(why) => this.note = Some(why.into()),
                }
                cx.emit(NotebookEvent::Changed);
                cx.notify();
            })
            .ok();
            done
        })
    }

    /// Its tab is in front, or no longer: extensions read which notebook
    /// is, and which cell of it the cursor is in.
    pub fn fronted(&self, front: bool, cx: &App) {
        let uri = front.then_some(self.uri.as_str());
        let params = json!({ "uri": uri, "selected": self.selected });
        self.tell("notebook.front", params, cx);
    }

    /// Its tab was closed.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        self.tell("notebook.close", json!({ "uri": self.uri }), cx);
        if let Some(store) = ExtensionStore::try_global(cx) {
            let (extension, uri) = (self.extension.clone(), self.uri.clone());
            store.update(cx, |store, _| store.notebook_closed(&extension, &uri));
        }
    }

    fn render_cell(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(cell) = self.cells.get(ix) else {
            return div().into_any_element();
        };
        let settings = Settings::get(cx);
        let (font, size) = (settings.buffer_font().family, settings.buffer_font_size());
        let selected = ix == self.selected;
        let handle = cell.handle as usize;
        let state: SharedString = match (cell.code, cell.running, cell.order) {
            (false, _, _) => "Text".into(),
            (true, true, _) => "Running".into(),
            (true, false, Some(order)) => format!("[{order}]").into(),
            (true, false, None) => "[ ]".into(),
        };
        let header = div()
            .h(px(26.))
            .px_1p5()
            .flex()
            .items_center()
            .gap_1p5()
            .bg(theme.bg_elev)
            .text_size(UI_FONT_SMALL)
            .text_color(theme.fg_subtle)
            .when(cell.code, |d| {
                let (name, running) = match cell.running {
                    true => ("stop", true),
                    false => ("play", false),
                };
                d.child(
                    icon(("cell-run", handle), name, &theme)
                        .debug_selector(move || format!("cell-run-{ix}"))
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| match running {
                                true => this.interrupt(&Interrupt, window, cx),
                                false => this.run_at(ix, cx),
                            },
                        )),
                )
            })
            .child(
                div()
                    .when(cell.failed && !cell.running, |d| d.text_color(theme.error))
                    .child(state),
            )
            .when(cell.code, |d| d.child(cell.language.clone()))
            .child(div().flex_1())
            .child(
                icon(("cell-up", handle), "arrow-up", &theme).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| this.move_at(ix, false, window, cx),
                )),
            )
            .child(
                icon(("cell-down", handle), "arrow-down", &theme).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| this.move_at(ix, true, window, cx),
                )),
            )
            .child(
                icon(("cell-delete", handle), "trash", &theme)
                    .debug_selector(move || format!("cell-delete-{ix}"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.delete_at(ix, window, cx)
                    })),
            );
        let put_out = (!cell.outputs.is_empty()).then(|| {
            div()
                .px_3()
                .py_1p5()
                .flex()
                .flex_col()
                .gap_1()
                .border_t(theme.shape.border)
                .border_color(theme.line)
                .bg(theme.bg_sunken)
                .font_family(font)
                .text_size(size)
                .children(cell.outputs.iter().map(|output| {
                    match output {
                        Output::Words(words) => {
                            div().text_color(theme.fg_muted).child(words.clone())
                        }
                        Output::Error(words) => div().text_color(theme.error).child(words.clone()),
                        Output::Unknown(what) => {
                            div().text_color(theme.fg_subtle).child(what.clone())
                        }
                        Output::Picture(picture) => div().child(
                            img(picture.clone())
                                .max_w(px(720.))
                                .max_h(px(480.))
                                .object_fit(ObjectFit::ScaleDown),
                        ),
                    }
                }))
        });
        div()
            .px_4()
            .pt_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(theme.shape.control)
                    .border(theme.shape.border)
                    .border_color(if selected { theme.accent } else { theme.line })
                    .overflow_hidden()
                    .child(header)
                    .child(cell.editor.clone())
                    .children(put_out),
            )
            .into_any_element()
    }
}

#[cfg(test)]
impl Notebook {
    pub(crate) fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready)
    }

    pub(crate) fn runner(&self) -> Option<String> {
        self.runner.as_ref().map(|runner| runner.to_string())
    }

    pub(crate) fn note(&self) -> Option<String> {
        self.note.as_ref().map(|note| note.to_string())
    }

    /// Each cell as a line: what it is, its text, and what is under it.
    pub(crate) fn seen(&self, cx: &App) -> Vec<String> {
        let line = |cell: &Cell| {
            let mut line = match cell.code {
                true => format!("code {}", cell.language),
                false => format!("text {}", cell.language),
            };
            if cell.running {
                line.push_str(" running");
            }
            if cell.failed {
                line.push_str(" failed");
            }
            match (cell.code, cell.order) {
                (true, Some(order)) => line.push_str(&format!(" [{order}]")),
                (true, None) => line.push_str(" [ ]"),
                (false, _) => {}
            }
            line.push_str(&format!(": {}", cell.editor.read(cx).text(cx)));
            for output in &cell.outputs {
                let first = |words: &SharedString| words.lines().next().unwrap_or("").to_string();
                line.push_str(&match output {
                    Output::Words(words) => format!(" => {}", first(words)),
                    Output::Error(words) => format!(" => ! {}", first(words)),
                    Output::Picture(_) => " => a picture".to_string(),
                    Output::Unknown(what) => format!(" => ? {what}"),
                });
            }
            line
        };
        self.cells.iter().map(line).collect()
    }
}

impl Render for Notebook {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Keys given to the notebook itself go to the cell the cursor was
        // in: a click beside the cells does not take them away.
        if self.focus_handle.is_focused(window)
            && let Some(cell) = self.cells.get(self.selected)
        {
            window.focus(&cell.editor.focus_handle(cx));
        }
        // The cell the cursor is in is the one things are done to.
        let focused = self.cells.iter().position(|cell| {
            let focus = cell.editor.focus_handle(cx);
            focus.contains_focused(window, cx)
        });
        if let Some(focused) = focused.filter(|focused| *focused != self.selected) {
            self.selected = focused;
            self.fronted(true, cx);
        }
        let running = self.cells.iter().any(|cell| cell.running);
        let runner: SharedString = match (&self.state, &self.runner) {
            (State::Ready, Some(runner)) => format!("Runs with {runner}").into(),
            (State::Ready, None) => "Nothing here runs its cells yet".into(),
            _ => "".into(),
        };
        let bar = div()
            .flex_none()
            .h(px(36.))
            .px_4()
            .flex()
            .items_center()
            .gap_1p5()
            .border_b(theme.shape.border)
            .border_color(theme.line)
            .text_size(UI_FONT_SIZE)
            .child(
                crate::ui::button(
                    "notebook-run-all",
                    "Run all",
                    false,
                    &theme,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.run_all(&RunAll, window, cx)
                    }),
                )
                .debug_selector(|| "notebook-run-all".into()),
            )
            .when(running, |d| {
                d.child(crate::ui::button(
                    "notebook-stop",
                    "Stop",
                    false,
                    &theme,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.interrupt(&Interrupt, window, cx)
                    }),
                ))
            })
            .child(
                crate::ui::button(
                    "notebook-add-code",
                    "Add code",
                    false,
                    &theme,
                    cx.listener(|this, _: &ClickEvent, window, cx| this.add(true, window, cx)),
                )
                .debug_selector(|| "notebook-add-code".into()),
            )
            .child(crate::ui::button(
                "notebook-add-text",
                "Add text",
                false,
                &theme,
                cx.listener(|this, _: &ClickEvent, window, cx| this.add(false, window, cx)),
            ))
            .child(div().flex_1())
            .children(self.note.clone().map(|note| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.error)
                    .child(note)
            }))
            .child(div().flex_none().text_color(theme.fg_subtle).child(runner));
        let said = |words: SharedString| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(UI_FONT_SIZE)
                .text_color(theme.fg_subtle)
                .child(words)
                .into_any_element()
        };
        let body = match &self.state {
            State::Reading => said("Reading the notebook".into()),
            State::Failed(why) => said(why.clone()),
            State::Ready if self.cells.is_empty() => said("No cells yet".into()),
            State::Ready => {
                let cell =
                    cx.processor(|this, ix: usize, window, cx| this.render_cell(ix, window, cx));
                list(self.list.clone(), cell)
                    .size_full()
                    .pb_2()
                    .into_any_element()
            }
        };
        div()
            .id("notebook")
            .key_context("Notebook")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .on_action(cx.listener(Self::run_cell))
            .on_action(cx.listener(Self::run_cell_and_next))
            .on_action(cx.listener(Self::run_all))
            .on_action(cx.listener(Self::interrupt))
            .on_action(cx.listener(Self::add_code))
            .on_action(cx.listener(Self::add_text))
            .on_action(cx.listener(Self::delete_cell))
            .on_action(cx.listener(Self::move_cell_up))
            .on_action(cx.listener(Self::move_cell_down))
            // A cell has no file of its own to be saved to: its editor
            // lets the key through, and the notebook is what is saved.
            .on_action(cx.listener(|this, _: &crate::editor::Save, _, cx| {
                this.save(cx).detach();
            }))
            .child(bar)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// A small button that is a picture, in the head of a cell.
fn icon(id: impl Into<ElementId>, name: &'static str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.shape.token)
        .hover(|d| d.bg(theme.line).text_color(theme.fg))
        .child(crate::icons::draw(name))
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// A file name of the kind a language is written in, which is what picks
/// the colors of a cell: an extension says a language by VS Code's name
/// for it.
fn language_file(language: &str) -> PathBuf {
    let ending = match language {
        "python" => "py",
        "javascript" => "js",
        "typescript" => "ts",
        "markdown" => "md",
        "shellscript" | "bash" => "sh",
        "rust" => "rs",
        "ruby" => "rb",
        "csharp" => "cs",
        "julia" => "jl",
        "powershell" => "ps1",
        "plaintext" => "txt",
        other => other,
    };
    PathBuf::from(format!("cell.{ending}"))
}

/// What a run put out, as the host said it: of each output the form that
/// can be drawn best. What went wrong first, then a picture, then words.
pub(crate) fn outputs(said: &Value) -> Vec<Output> {
    const ERROR: &str = "application/vnd.code.notebook.error";
    const STDERR: &str = "application/vnd.code.notebook.stderr";
    const STDOUT: &str = "application/vnd.code.notebook.stdout";
    let one = |output: &Value| -> Option<Output> {
        let items: Vec<&Value> = output["items"].as_array()?.iter().collect();
        let of = |mime: &str| items.iter().find(|item| item["mime"] == mime).copied();
        let words = |item: &Value| item["text"].as_str().map(cut);
        if let Some(words) = of(ERROR).or(of(STDERR)).and_then(words) {
            return Some(Output::Error(words));
        }
        let picture = items.iter().find_map(|item| {
            let format = match item["mime"].as_str()? {
                "image/png" => ImageFormat::Png,
                "image/jpeg" => ImageFormat::Jpeg,
                "image/gif" => ImageFormat::Gif,
                "image/webp" => ImageFormat::Webp,
                _ => return None,
            };
            let bytes = unbase64(item["picture"].as_str()?)?;
            Some(Arc::new(Image::from_bytes(format, bytes)))
        });
        if let Some(picture) = picture {
            return Some(Output::Picture(picture));
        }
        // Plain words before words in a form of their own, and a page
        // (HTML) never as its source.
        let plain = of(STDOUT).or(of("text/plain")).or_else(|| {
            let worded = |item: &&&Value| item["text"].is_string() && item["mime"] != "text/html";
            items.iter().find(worded).copied()
        });
        if let Some(words) = plain.and_then(words) {
            return Some(Output::Words(words));
        }
        let first = items.first()?;
        let (mime, size) = (text(&first["mime"]), first["size"].as_u64().unwrap_or(0));
        Some(Output::Unknown(
            format!("{mime}, {}: not drawn here", size_of(size)).into(),
        ))
    };
    said.as_array()
        .into_iter()
        .flatten()
        .filter_map(one)
        .collect()
}

/// The words of an output as they are drawn: without the line break they
/// end with, and cut where they are longer than is read under a cell.
fn cut(words: &str) -> SharedString {
    let words = words.trim_end_matches(['\n', '\r']);
    let mut end = words.len().min(OUTPUT_BYTES);
    while !words.is_char_boundary(end) {
        end -= 1;
    }
    let lines = words[..end].split('\n').count();
    if end == words.len() && lines <= OUTPUT_LINES {
        return words.to_string().into();
    }
    let kept: Vec<&str> = words[..end].split('\n').take(OUTPUT_LINES).collect();
    let left = words.split('\n').count() - kept.len();
    match left {
        0 => format!("{}\n(cut here)", kept.join("\n")).into(),
        left => format!("{}\n({left} more lines)", kept.join("\n")).into(),
    }
}

fn size_of(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

/// The bytes a text in base64 stands for.
fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut held, mut bits) = (0u32, 0u32);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        held = held << 6 | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((held >> bits) as u8);
            held &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn of_each_output_the_form_that_can_be_drawn_is_taken() {
        let said = json!([
            { "items": [
                { "mime": "text/html", "size": 30, "text": "<b>3</b>" },
                { "mime": "text/plain", "size": 1, "text": "3\n" },
            ] },
            { "items": [
                { "mime": "text/plain", "size": 6, "text": "<Figure>" },
                { "mime": "image/png", "size": 4, "picture": "iVBORw==" },
            ] },
            { "items": [{ "mime": "application/vnd.code.notebook.error", "size": 9, "text": "Error: no" }] },
            { "items": [{ "mime": "text/html", "size": 2048, "text": "<table></table>" }] },
            { "items": [{ "mime": "text/markdown", "size": 3, "text": "# A" }] },
            { "items": [] },
        ]);
        let drawn: Vec<String> = outputs(&said)
            .iter()
            .map(|output| match output {
                Output::Words(words) => format!("words {words}"),
                Output::Error(words) => format!("error {words}"),
                Output::Picture(picture) => format!("picture {:?}", picture.bytes()),
                Output::Unknown(what) => format!("unknown {what}"),
            })
            .collect();
        assert_eq!(
            drawn,
            [
                "words 3",
                "picture [137, 80, 78, 71]",
                "error Error: no",
                "unknown text/html, 2.0 KB: not drawn here",
                "words # A",
            ]
        );
    }

    #[test]
    fn a_long_output_is_cut() {
        let long = "line\n".repeat(OUTPUT_LINES + 50);
        let cut = cut(&long);
        assert_eq!(cut.split('\n').count(), OUTPUT_LINES + 1);
        assert!(
            cut.ends_with("\n(50 more lines)"),
            "{}",
            &cut[cut.len() - 30..]
        );
        assert_eq!(super::cut("a\nb\n").as_ref(), "a\nb");
        let wide = "é".repeat(OUTPUT_BYTES);
        assert!(super::cut(&wide).ends_with("\n(cut here)"));
    }

    #[test]
    fn base64_is_read() {
        assert_eq!(unbase64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(unbase64("aGVsbG8h").unwrap(), b"hello!");
        assert_eq!(unbase64("").unwrap(), b"");
        assert!(unbase64("a b").is_none());
    }
}
