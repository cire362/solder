//! A notebook: a file an extension reads as a list of cells, some of them
//! code it runs.
//!
//! The extension does what is its own: it reads the file into cells and
//! writes them back, and it runs the code (`host/notebooks.js`). What is
//! on screen is here: a tab with the cells one under another, each an
//! editor as tall as its text, and under a cell what its last run put
//! out. Words and pictures are drawn inline; HTML and an allowed renderer
//! are opened on demand in a separate page tab.

use crate::{
    document::{Document, DocumentEvent},
    editor::{Editor, Reveal},
    extension_store::ExtensionStore,
    settings::Settings,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE, UI_FONT_SMALL},
};
use futures::{FutureExt, future::Shared};
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
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
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
        MoveCellDown,
        ExportCopy
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
    Rich(Arc<Value>, Option<SharedString>),
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
    Output {
        title: String,
        uri: String,
        items: Arc<Value>,
        renderer: Option<Box<(String, PathBuf, extension::NotebookRenderer)>>,
    },
}

pub struct Notebook {
    pub path: PathBuf,
    /// The extension that reads it, and what it calls this kind.
    extension: String,
    kind: String,
    uri: String,
    state: State,
    loading_generation: u64,
    recovered_generation: Option<u64>,
    restoration_cancelled: Option<Arc<AtomicBool>>,
    client_tag: Option<String>,
    host_version: Option<u64>,
    cells: Vec<Cell>,
    next_handle: u64,
    /// The cell the cursor is in, or was in last.
    selected: usize,
    dirty: bool,
    generation: u64,
    pending_saves: usize,
    save_tail: Option<Shared<Task<bool>>>,
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
        let lifecycle = format!("view-{}", cx.entity_id());
        let reading = (!extension.is_empty())
            .then(|| ExtensionStore::try_global(cx))
            .flatten()
            .map(|store| {
                store.update(cx, |store, cx| {
                    store.notebook_opened(&extension, &uri, me);
                    let params = json!({ "type": kind, "uri": uri, "lifecycle":lifecycle });
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
                this.update(cx, |this, cx| {
                    if this.loading_generation != 0 {
                        return;
                    }
                    match read {
                        Ok((file, read)) => {
                            this.ipynb = Some(file);
                            this.read(Ok(read), cx);
                        }
                        Err(why) => this.read(Err(why), cx),
                    }
                })
                .ok();
                return;
            }
            let read = match reading {
                Some(reading) => reading.await,
                None => Err(crate::lang::t("Extensions are not there to read it").into()),
            };
            this.update(cx, |this, cx| {
                if this.loading_generation == 0 {
                    this.read(read, cx);
                }
            })
            .ok();
        })
        .detach();
        Self {
            path,
            extension,
            kind,
            uri,
            state: State::Reading,
            loading_generation: 0,
            recovered_generation: None,
            restoration_cancelled: None,
            client_tag: None,
            host_version: None,
            cells: Vec::new(),
            next_handle: 0,
            selected: 0,
            dirty: false,
            generation: 0,
            pending_saves: 0,
            save_tail: None,
            ipynb: None,
            runner: None,
            note: None,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Captures only cheap rope/model handles on this thread. Flattening,
    /// notebook JSON and disk writes belong to the recovery writer's workers.
    pub(crate) fn recovery_snapshot(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Value, String>>> {
        if !matches!(self.state, State::Ready) {
            return None;
        }
        if self
            .cells
            .iter()
            .map(|cell| cell.editor.read(cx).doc(cx).text().len())
            .sum::<usize>()
            > 16 * 1024 * 1024
        {
            return None;
        }
        if let Some(file) = &self.ipynb {
            let checkpoint = file.checkpoint()?;
            let cells = self
                .cells
                .iter()
                .map(|cell| {
                    (
                        cell.handle,
                        cell.code,
                        cell.editor.read(cx).doc(cx).text().rope().clone(),
                    )
                })
                .collect::<Vec<_>>();
            Some(cx.background_executor().spawn(async move {
                let cells = cells
                    .into_iter()
                    .map(|(handle, code, text)| (handle, code, text.to_string()))
                    .collect::<Vec<_>>();
                Ok(checkpoint.snapshot(&cells))
            }))
        } else {
            Some(self.ask("notebook.snapshot", json!({"uri":self.uri}), cx))
        }
    }

    pub(crate) fn recovered_generation(&self) -> Option<u64> {
        self.recovered_generation
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// A live-disk read begun by session restoration must not replace this
    /// recovery later. Permission is still checked by the extension store.
    pub(crate) fn restore(
        &mut self,
        snapshot: Value,
        selected: usize,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        if self.is_dirty() || self.restoration_cancelled.is_some() {
            return Task::ready(false);
        }
        self.state = State::Reading;
        for cell in &self.cells {
            let document = cell.editor.read(cx).document().clone();
            document.update(cx, |document, cx| document.lock_cell(true, cx));
        }
        self.loading_generation += 1;
        let ticket = self.loading_generation;
        let generation = self.generation;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.restoration_cancelled = Some(cancelled.clone());
        let native = self.extension.is_empty();
        let (native_snapshot, asked) = if native {
            (Some(snapshot), None)
        } else {
            let mut params = json!({"uri":self.uri,"type":self.kind,"lifecycle":format!("view-{}", cx.entity_id())});
            params["snapshot"] = snapshot;
            let asked = match ExtensionStore::try_global(cx) {
                Some(store) => store.update(cx, |store, cx| {
                    store.ask_host_guarded(
                        &self.extension,
                        "notebook.restore",
                        params,
                        Some(cancelled.clone()),
                        cx,
                    )
                }),
                None => Task::ready(Err(crate::lang::t("Extensions are not there").into())),
            };
            (None, Some(asked))
        };
        cx.spawn(async move |this, cx| {
            let result = if native {
                cx.background_executor().spawn(async move {
                    crate::ipynb::File::restore(&native_snapshot.unwrap()).map(|(file, shown)| (Some(file), shown))
                }).await
            } else {
                asked.unwrap().await.map(|shown| (None, shown))
            };
            this.update(cx, |this, cx| {
                if this.loading_generation != ticket || cancelled.load(Ordering::Acquire) {
                    return false;
                }
                this.restoration_cancelled = None;
                match result {
                    Ok((file, shown)) => {
                        this.cells.clear(); this.next_handle = 0;
                        this.ipynb = file;
                        this.read(Ok(shown), cx);
                        this.selected = selected.min(this.cells.len().saturating_sub(1));
                        this.dirty = true; this.generation = this.generation.max(generation) + 1;
                        this.recovered_generation = Some(this.generation);
                        this.note = Some(crate::lang::t("This notebook was recovered. Its copy is kept until you save or close it.").into());
                        cx.emit(NotebookEvent::Changed); cx.notify(); true
                    }
                    Err(why) => {
                        for cell in &this.cells {
                            let document = cell.editor.read(cx).document().clone();
                            document.update(cx, |document, cx| document.lock_cell(false, cx));
                        }
                        if !this.cells.is_empty() { this.state = State::Ready; }
                        if matches!(this.state, State::Reading) { this.state = State::Failed(why.clone().into()); }
                        this.note = Some(why.into()); cx.notify(); false
                    }
                }
            }).unwrap_or(false)
        })
    }

    pub(crate) fn export_copy(&mut self, _: &ExportCopy, _: &mut Window, cx: &mut Context<Self>) {
        let parent = self.path.parent().unwrap_or(Path::new("."));
        let stem = self.path.file_stem().unwrap_or_default().to_string_lossy();
        let extension = self.path.extension().unwrap_or_default().to_string_lossy();
        let name = if extension.is_empty() {
            format!("{stem}.copy")
        } else {
            format!("{stem}.copy.{extension}")
        };
        let asked = cx.prompt_for_new_path(parent, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = asked.await else {
                return;
            };
            let Ok(exported) = this.update(cx, |this, cx| this.export(path, cx)) else {
                return;
            };
            exported.await;
        })
        .detach();
    }

    /// Export is a copy. It does not mark the original notebook saved or
    /// transfer its source revision to the chosen destination.
    pub(crate) fn export(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<bool> {
        if !matches!(self.state, State::Ready) {
            return Task::ready(false);
        }
        let source = self.path.clone();
        let native = self.ipynb.clone();
        let cells = self
            .cells
            .iter()
            .map(|cell| {
                (
                    cell.handle,
                    cell.code,
                    cell.editor.read(cx).doc(cx).text().rope().clone(),
                )
            })
            .collect::<Vec<_>>();
        let target_uri = crate::extension_api::uri(&path);
        let prepared = native.is_none().then(|| {
            self.ask(
                "notebook.export.prepare",
                json!({"uri":self.uri,"target":target_uri}),
                cx,
            )
        });
        let native_destination = native.as_ref().map(|file| {
            let (file, source, target) = (file.clone(), source.clone(), path.clone());
            cx.background_executor()
                .spawn(async move { copy_revision(&file, &source, &target) })
        });
        let uri = self.uri.clone();
        let previous = self.save_tail.take();
        self.pending_saves += 1;
        cx.emit(NotebookEvent::Changed);
        cx.notify();
        let exporting = cx
            .spawn(async move |this, cx| {
                if let Some(previous) = previous {
                    previous.await;
                }
                let result = if let Some(file) = native {
                    let target = path.clone();
                    cx.background_executor()
                        .spawn(async move {
                            let stamp = native_destination.unwrap().await?;
                            if copy_revision(&file, &source, &target)? != stamp {
                                return Err(crate::lang::t(
                                    "The notebook copy destination changed. Export again.",
                                )
                                .into());
                            }
                            let cells = cells
                                .into_iter()
                                .map(|(handle, code, text)| (handle, code, text.to_string()))
                                .collect::<Vec<_>>();
                            file.export(&target, &cells, stamp.as_ref())
                        })
                        .await
                } else {
                    match prepared.unwrap().await {
                        Ok(token) => {
                            match this.update(cx, |this, cx| {
                                this.ask(
                                    "notebook.export",
                                    json!({"uri":uri,"target":target_uri,"token":token}),
                                    cx,
                                )
                            }) {
                                Ok(asked) => asked.await.map(|_| ()),
                                Err(_) => return false,
                            }
                        }
                        Err(why) => Err(why),
                    }
                };
                let done = result.is_ok();
                this.update(cx, |this, cx| {
                    this.pending_saves -= 1;
                    this.note = Some(match result {
                        Ok(()) => {
                            crate::lang::tf("Notebook copy exported to {}", &[&path.display()])
                                .into()
                        }
                        Err(why) => why.into(),
                    });
                    cx.emit(NotebookEvent::Changed);
                    cx.notify();
                })
                .ok();
                done
            })
            .shared();
        self.save_tail = Some(exporting.clone());
        cx.spawn(async move |_, _| exporting.await)
    }

    pub fn title(&self) -> String {
        let name = self.path.file_name();
        name.map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty || self.is_saving()
    }

    pub(crate) fn is_saving(&self) -> bool {
        self.pending_saves != 0
    }

    pub(crate) fn pending_save(&self) -> Option<Shared<Task<bool>>> {
        self.is_saving().then(|| self.save_tail.clone()).flatten()
    }

    pub(crate) fn version(&self) -> u64 {
        self.generation
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
                self.client_tag = read["client_tag"].as_str().map(str::to_owned);
                self.host_version = read["version"].as_u64();
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
                    made.failed = cell["failed"] == true;
                    self.cells.push(made);
                }
                self.list.reset(self.cells.len());
                self.state = State::Ready;
                if read["dirty"] == true {
                    self.dirty = true;
                    cx.emit(NotebookEvent::Changed);
                }
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
            None => Task::ready(Err(crate::lang::t("Extensions are not there").into())),
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
        self.changed(cx);
        self.client_tag = Some(format!("{}:{}", cx.entity_id(), self.generation));
        let params = json!({ "uri": self.uri, "handle": handle, "value": value, "client_tag":self.client_tag });
        self.tell("notebook.cell", params, cx);
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
        self.changed(cx);
        self.client_tag = Some(format!("{}:{}", cx.entity_id(), self.generation));
        self.tell(
            "notebook.cells",
            json!({ "uri": self.uri, "cells": cells, "client_tag":self.client_tag }),
            cx,
        );
    }

    /// Something its extension said of it.
    pub(crate) fn heard(&mut self, method: &str, params: &Value, cx: &mut Context<Self>) {
        let handle = params["handle"].as_u64();
        let cell = self
            .cells
            .iter_mut()
            .find(|cell| Some(cell.handle) == handle);
        match (method, cell) {
            ("notebook.version", _) => {
                if params["client_tag"].as_str() == self.client_tag.as_deref() {
                    self.host_version = params["version"].as_u64();
                }
            }
            ("notebook.outputs", Some(cell)) => {
                cell.outputs = outputs(&params["outputs"]);
                self.host_version = params["version"].as_u64();
                self.changed(cx);
            }
            ("notebook.run", Some(cell)) => {
                cell.running = params["running"] == true;
                if !cell.running {
                    cell.failed = params["failed"] == true;
                    cell.order = params["order"].as_u64().or(cell.order);
                    self.host_version = params["version"].as_u64();
                    self.changed(cx);
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
                if params["client_tag"].as_str() == self.client_tag.as_deref()
                    && params["version"].as_u64() == self.host_version
                {
                    self.dirty = false;
                    cx.emit(NotebookEvent::Changed);
                }
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
        self.note = Some(
            crate::lang::t("Its extension stopped. Open the file again to run or save it.").into(),
        );
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
        if !matches!(self.state, State::Ready) {
            return;
        }
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
        if !matches!(self.state, State::Ready) {
            return;
        }
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
        if !matches!(self.state, State::Ready) {
            return;
        }
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
        if !matches!(self.state, State::Ready) {
            return Task::ready(false);
        }
        let generation = self.generation;
        let native = self.ipynb.clone().map(|file| {
            let cells = self
                .cells
                .iter()
                .map(|cell| (cell.handle, cell.code, cell.editor.read(cx).text(cx)))
                .collect::<Vec<_>>();
            (file, self.path.clone(), cells)
        });
        let previous = self.save_tail.take();
        self.pending_saves += 1;
        self.note = None;
        cx.emit(NotebookEvent::Changed);
        cx.notify();
        let saving = cx
            .spawn(async move |this, cx| {
                // Capture the cells at the request, but start their write after
                // earlier snapshots. A late older save must not replace a new one.
                if let Some(previous) = previous {
                    previous.await;
                }
                let saved = match native {
                    Some((file, path, cells)) => {
                        cx.background_executor()
                            .spawn(
                                async move { file.save(&path, &cells).map(|_| Value::Bool(true)) },
                            )
                            .await
                    }
                    None => match this.update(cx, |this, cx| {
                        this.ask("notebook.save", json!({ "uri": this.uri }), cx)
                    }) {
                        Ok(asked) => asked.await,
                        Err(_) => return false,
                    },
                };
                let done = saved.is_ok();
                this.update(cx, |this, cx| {
                    this.pending_saves -= 1;
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
            .shared();
        self.save_tail = Some(saving.clone());
        cx.spawn(async move |_, _| saving.await)
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
        self.loading_generation += 1;
        if let Some(cancelled) = self.restoration_cancelled.take() {
            cancelled.store(true, Ordering::Release);
        }
        self.tell(
            "notebook.close",
            json!({ "uri": self.uri, "lifecycle":format!("view-{}",cx.entity_id()) }),
            cx,
        );
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
            (false, _, _) => crate::lang::t("Text").into(),
            (true, true, _) => crate::lang::t("Running").into(),
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
                .children(cell.outputs.iter().enumerate().map(|(output_ix, output)| {
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
                        Output::Rich(items, fallback) => {
                            let renderer = ExtensionStore::try_global(cx).and_then(|store| {
                                let store = store.read(cx);
                                store
                                    .installed
                                    .iter()
                                    .filter(|extension| store.may_render(extension))
                                    .find_map(|extension| {
                                        extension
                                            .notebook_renderers
                                            .iter()
                                            .find(|renderer| {
                                                (!renderer.messaging || extension.node().is_some())
                                                    && items.as_array().into_iter().flatten().any(
                                                        |item| {
                                                            renderer.mimes.iter().any(|mime| {
                                                                item["mime"] == mime.as_str()
                                                            })
                                                        },
                                                    )
                                            })
                                            .map(|renderer| {
                                                (
                                                    extension.id.clone(),
                                                    extension.dir.clone(),
                                                    renderer.clone(),
                                                )
                                            })
                                    })
                            });
                            let html = items
                                .as_array()
                                .into_iter()
                                .flatten()
                                .any(|item| item["mime"] == "text/html");
                            let mut result = div().children(
                                fallback
                                    .clone()
                                    .map(|words| div().text_color(theme.fg_muted).child(words)),
                            );
                            if renderer.is_some() || html {
                                let items = items.clone();
                                let label = renderer
                                    .as_ref()
                                    .map(|(_, _, renderer)| {
                                        crate::lang::tf(
                                            "View with {}",
                                            &[&if renderer.name.is_empty() {
                                                &renderer.id
                                            } else {
                                                &renderer.name
                                            }],
                                        )
                                    })
                                    .unwrap_or_else(|| crate::lang::t("View HTML output").into());
                                result = result.child(
                                    crate::ui::button(
                                        ("rich-output", handle * 1000 + output_ix),
                                        label,
                                        false,
                                        &theme,
                                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            cx.emit(NotebookEvent::Output {
                                                title: crate::lang::tf(
                                                    "{}: output {}",
                                                    &[&this.title(), &(ix + 1)],
                                                ),
                                                uri: this.uri.clone(),
                                                items: items.clone(),
                                                renderer: renderer.clone().map(Box::new),
                                            });
                                        }),
                                    )
                                    .debug_selector(
                                        move || format!("notebook-output-{ix}-{output_ix}"),
                                    ),
                                );
                            } else {
                                result = result.child(div().text_color(theme.fg_subtle).child(
                                    crate::lang::t("No renderer is allowed for this output"),
                                ));
                            }
                            result
                        }
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
                    Output::Rich(_, fallback) => fallback
                        .as_ref()
                        .map(|words| format!(" => {}", first(words)))
                        .unwrap_or_else(|| " => an interactive output".into()),
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
            (State::Ready, Some(runner)) => crate::lang::tf("Runs with {}", &[&runner]).into(),
            (State::Ready, None) => crate::lang::t("Nothing here runs its cells yet").into(),
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
                    crate::lang::t("Run all"),
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
                    crate::lang::t("Stop"),
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
                    crate::lang::t("Add code"),
                    false,
                    &theme,
                    cx.listener(|this, _: &ClickEvent, window, cx| this.add(true, window, cx)),
                )
                .debug_selector(|| "notebook-add-code".into()),
            )
            .child(crate::ui::button(
                "notebook-add-text",
                crate::lang::t("Add text"),
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
                    .child(crate::extension_store::error_label(&note))
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
            State::Reading => said(crate::lang::t("Reading the notebook").into()),
            State::Failed(why) => said(crate::extension_store::error_label(why).into()),
            State::Ready if self.cells.is_empty() => said(crate::lang::t("No cells yet").into()),
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
            .on_action(cx.listener(Self::export_copy))
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
        let rich = items.iter().any(|item| {
            let mime = item["mime"].as_str().unwrap_or_default();
            (mime == "text/html"
                || (!mime.starts_with("text/")
                    && !matches!(mime, "application/json" | STDOUT | STDERR | ERROR)))
                && (item["text"].is_string() || item["data"].is_string())
        });
        if rich {
            return Some(Output::Rich(
                Arc::new(Value::Array(
                    items.iter().map(|item| (*item).clone()).collect(),
                )),
                of("text/plain").and_then(words),
            ));
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
            crate::lang::tf("{}, {}: not drawn here", &[&mime, &size_of(size)]).into(),
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
        0 => crate::lang::tf("{}\n(cut here)", &[&kept.join("\n")]).into(),
        left => crate::lang::tf("{}\n({} more lines)", &[&kept.join("\n"), &left]).into(),
    }
}

fn size_of(bytes: u64) -> String {
    match bytes {
        0..1024 => crate::lang::tf("{} B", &[&bytes]),
        1024..1_048_576 => crate::lang::tf("{} KB", &[&format!("{:.1}", bytes as f64 / 1024.)]),
        _ => crate::lang::tf("{} MB", &[&format!("{:.1}", bytes as f64 / 1_048_576.)]),
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

/// Blocking: capture a chosen copy destination without granting source aliases.
fn copy_revision(
    file: &crate::ipynb::File,
    source: &Path,
    target: &Path,
) -> Result<Option<crate::remote::Stamp>, String> {
    let current_alias = match crate::remote::same_file(source, target) {
        Ok(alias) => alias,
        Err(_)
            if file.original_source().is_some()
                && crate::remote::metadata(source)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            false
        }
        Err(error) => return Err(error.to_string()),
    };
    let original_alias = match file.original_source() {
        Some(source) => {
            crate::remote::same_file(source, target).map_err(|error| error.to_string())?
        }
        None => false,
    };
    let stamp = crate::remote::revision(target).map_err(|error| error.to_string())?;
    if current_alias || original_alias || (file.original_source().is_none() && stamp.is_some()) {
        return Err(crate::lang::t("Choose another path for the notebook copy").into());
    }
    Ok(stamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    async fn restoration_locks_cells_and_export_protects_the_original_symlink_target(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        cx.update(|cx| cx.set_global(Settings::default()));
        let root = db::testing::dir("notebook-restore-lock");
        let path = root.join("book.ipynb");
        let bytes = serde_json::to_vec(&json!({"nbformat":4,"metadata":{},"cells":[
            {"cell_type":"code","source":"original","metadata":{},"outputs":[],"execution_count":null}]})).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let (file, _) = crate::ipynb::File::read(&path).unwrap();
        let snapshot = file
            .checkpoint()
            .unwrap()
            .snapshot(&[(0, true, "recovered".into())]);
        let book =
            cx.new(|cx| Notebook::open(path.clone(), String::new(), "jupyter-notebook".into(), cx));
        for _ in 0..crate::patience::LOOKS {
            cx.run_until_parked();
            if book.read_with(cx, |book, _| book.is_ready()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(book.read_with(cx, |book, _| book.is_ready()));
        let editor = book.read_with(cx, |book, _| book.cells[0].editor.clone());
        let document = editor.read_with(cx, |ed, _| ed.document().clone());
        let restoring = book.update(cx, |book, cx| book.restore(snapshot, 0, cx));
        document.update(cx, |doc, cx| {
            assert!(doc.is_read_only());
            assert!(
                doc.edit(
                    vec![(0..0, "late".into())],
                    &[],
                    Some(editor.entity_id()),
                    cx
                )
                .is_empty()
            );
            assert!(doc.undo(editor.entity_id(), cx).is_none());
            assert!(doc.redo(editor.entity_id(), cx).is_none());
        });
        assert!(restoring.await);
        assert_eq!(
            book.read_with(cx, |book, cx| book.cells[0].editor.read(cx).text(cx)),
            "recovered"
        );
        #[cfg(unix)]
        {
            let alias = root.join("link.ipynb");
            std::os::unix::fs::symlink(&path, &alias).unwrap();
            book.update(cx, |book, _| book.path = alias.clone());
            std::fs::remove_file(&alias).unwrap();
            // The old physical target stays protected even with a dangling view path.
            assert!(
                !book
                    .update(cx, |book, cx| book.export(path.clone(), cx))
                    .await
            );
            let rescue = root.join("copy.ipynb");
            assert!(
                book.update(cx, |book, cx| book.export(rescue.clone(), cx))
                    .await
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(
                serde_json::from_slice::<Value>(&std::fs::read(rescue).unwrap()).unwrap()["cells"]
                    [0]["source"],
                "recovered"
            );
        }
        assert!(book.read_with(cx, |book, _| book.is_dirty()));
    }

    #[gpui::test]
    async fn failed_restoration_unlocks_cells_and_late_saved_messages_keep_new_outputs(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        cx.update(|cx| cx.set_global(Settings::default()));
        let path = db::testing::dir("notebook-restore-failure").join("book.ipynb");
        std::fs::write(&path, serde_json::to_vec(&json!({"nbformat":4,"metadata":{},"cells":[
            {"cell_type":"code","source":"original","metadata":{},"outputs":[],"execution_count":null}]})).unwrap()).unwrap();
        let book = cx.new(|cx| Notebook::open(path, String::new(), "jupyter-notebook".into(), cx));
        for _ in 0..crate::patience::LOOKS {
            cx.run_until_parked();
            if book.read_with(cx, |book, _| book.is_ready()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !book
                .update(cx, |book, cx| book.restore(Value::Null, 0, cx))
                .await
        );
        book.update(cx, |book, cx| {
            assert!(book.is_ready());
            assert!(!book.cells[0].editor.read(cx).doc(cx).is_read_only());
            book.client_tag = Some("edit-1".into());
            book.heard(
                "notebook.outputs",
                &json!({"handle":0,"outputs":[],"version":4}),
                cx,
            );
            book.heard(
                "notebook.saved",
                &json!({"client_tag":"edit-1","version":3}),
                cx,
            );
            assert!(book.is_dirty());
            book.heard(
                "notebook.saved",
                &json!({"client_tag":"edit-old","version":4}),
                cx,
            );
            assert!(book.is_dirty());
            book.heard(
                "notebook.saved",
                &json!({"client_tag":"edit-1","version":4}),
                cx,
            );
            assert!(!book.is_dirty());
        });
    }

    #[gpui::test]
    async fn queued_notebook_saves_write_snapshots_in_order(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            cx.set_global(Settings::default());
        });
        let path = db::testing::dir("notebook-save-order").join("notes.ipynb");
        std::fs::write(&path, serde_json::to_vec(&json!({"nbformat":4,"nbformat_minor":5,"metadata":{},
            "cells":[{"cell_type":"code","source":"original","metadata":{},"execution_count":null,"outputs":[]}]})).unwrap()).unwrap();
        let book =
            cx.new(|cx| Notebook::open(path.clone(), String::new(), "jupyter-notebook".into(), cx));
        for _ in 0..crate::patience::LOOKS {
            cx.run_until_parked();
            if book.read_with(cx, |book, _| book.is_ready()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(book.read_with(cx, |book, _| book.is_ready()));
        let editor = book.read_with(cx, |book, _| book.cells[0].editor.clone());
        let document = editor.read_with(cx, |editor, _| editor.document().clone());
        let (release, blocked) = futures::channel::oneshot::channel();
        book.update(cx, |book, cx| {
            book.save_tail = Some(
                cx.spawn(async move |_, _| blocked.await.unwrap_or(false))
                    .shared(),
            );
        });
        document.update(cx, |doc, cx| {
            doc.edit(
                vec![(0..0, "first ".into())],
                &[],
                Some(editor.entity_id()),
                cx,
            );
        });
        let first = book.update(cx, |book, cx| book.save(cx));
        document.update(cx, |doc, cx| {
            doc.edit(
                vec![(0..0, "second ".into())],
                &[],
                Some(editor.entity_id()),
                cx,
            );
        });
        let second = book.update(cx, |book, cx| book.save(cx));
        cx.run_until_parked();
        assert!(book.read_with(cx, |book, _| book.is_saving() && book.is_dirty()));
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap()["cells"][0]["source"],
            "original"
        );
        release.send(true).unwrap();
        assert!(first.await);
        assert!(second.await);
        assert!(!book.read_with(cx, |book, _| book.is_saving() || book.is_dirty()));
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap()["cells"][0]["source"],
            "second first original"
        );
        let destination = path.with_file_name("copy.ipynb");
        std::fs::write(&destination, b"chosen destination").unwrap();
        let (release, blocked) = futures::channel::oneshot::channel();
        book.update(cx, |book, cx| {
            book.save_tail = Some(
                cx.spawn(async move |_, _| blocked.await.unwrap_or(false))
                    .shared(),
            );
        });
        let exported = book.update(cx, |book, cx| book.export(destination.clone(), cx));
        cx.run_until_parked();
        assert!(book.read_with(cx, |book, _| book.is_saving()));
        std::fs::write(&destination, b"external edit during queue wait").unwrap();
        release.send(true).unwrap();
        assert!(!exported.await);
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"external edit during queue wait"
        );
    }

    #[test]
    fn localized_output_notices_keep_the_program_output_and_mime() {
        let value = json!([
            {"items":[{"mime":"application/vnd.code.notebook.error","text":"Error: provider detail"}]},
            {"items":[{"mime":"application/custom","size":2048}]}
        ]);
        crate::lang::set(crate::lang::Language::Russian);
        let shown = outputs(&value);
        assert!(
            matches!(&shown[0], Output::Error(words) if words.as_str() == "Error: provider detail")
        );
        assert!(
            matches!(&shown[1], Output::Unknown(words) if words.as_str() == "application/custom, 2.0 КБ: здесь не отображается")
        );
        let long = "provider line\n".repeat(OUTPUT_LINES + 2);
        let clipped = cut(&long);
        assert!(clipped.starts_with("provider line\n"));
        assert!(clipped.ends_with("\n(ещё строк: 2)"));
        assert_eq!(
            value[0]["items"][0]["mime"],
            "application/vnd.code.notebook.error"
        );
        crate::lang::set(crate::lang::Language::English);
        assert!(
            matches!(&outputs(&value)[1], Output::Unknown(words) if words.as_str() == "application/custom, 2.0 KB: not drawn here")
        );
    }

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
                Output::Rich(_, _) => "rich output".into(),
            })
            .collect();
        assert_eq!(
            drawn,
            [
                "rich output",
                "picture [137, 80, 78, 71]",
                "error Error: no",
                "rich output",
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
