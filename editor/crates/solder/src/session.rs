//! Project tabs and view positions. Only identities are persisted for
//! extension pages: their serializer recreates the page in a new host.
use super::*;
use gpui::point;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::mpsc;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct File {
    path: PathBuf,
    cursors: Vec<(usize, usize)>,
    scroll: (f32, f32),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct Book {
    path: PathBuf,
    extension: String,
    kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Page {
    extension: String,
    view_type: String,
    document_uri: Option<String>,
    title: String,
    state: Value,
    column: i32,
    options: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct Session {
    version: u32,
    root: PathBuf,
    panes: Vec<Vec<File>>,
    active: Vec<Option<usize>>,
    active_pane: usize,
    books: Vec<Book>,
    pages: Vec<Page>,
    front_book: Option<usize>,
    front_page: Option<usize>,
}

pub(super) struct Writer(mpsc::Sender<(Session, futures::channel::oneshot::Sender<bool>)>);

impl Writer {
    fn new(path: PathBuf) -> Self {
        let (send, receive) = mpsc::channel::<(Session, futures::channel::oneshot::Sender<bool>)>();
        std::thread::spawn(move || {
            for (session, reply) in receive {
                let result = serde_json::to_vec(&session)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| crate::ipynb::write_atomic(&path, &bytes));
                if let Err(error) = &result {
                    eprintln!("could not save tabs: {error}");
                }
                let _ = reply.send(result.is_ok());
            }
        });
        Self(send)
    }

    fn send(&self, session: Session, cx: &App) -> Task<bool> {
        let (send, receive) = futures::channel::oneshot::channel();
        if self.0.send((session, send)).is_err() {
            return Task::ready(false);
        }
        cx.background_executor()
            .spawn(async move { receive.await.unwrap_or(false) })
    }
}

fn path(root: &Path) -> PathBuf {
    // A stable name across processes and platforms, with the full root
    // checked again when reading; no paths become directory names.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in root.to_string_lossy().bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    settings::config_dir()
        .join("sessions")
        .join(format!("{hash:016x}.json"))
}

fn read(path: &Path, root: &Path) -> Option<Session> {
    if std::fs::metadata(path).ok()?.len() > 4 * 1024 * 1024 {
        return None;
    }
    let session: Session = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (session.version == 1
        && session.root == root
        && session.panes.len() <= 8
        && session.panes.iter().all(|pane| pane.len() <= 256)
        && session.books.len() <= 128
        && session.pages.len() <= 128)
        .then_some(session)
}

impl Workspace {
    pub(super) fn snapshot(&self, cx: &App) -> Session {
        let panes = self
            .panes
            .iter()
            .map(|pane| {
                pane.tabs
                    .iter()
                    .filter_map(|tab| {
                        let editor = tab.editor.read(cx);
                        Some(File {
                            path: editor.path(cx)?.to_path_buf(),
                            cursors: editor
                                .selections
                                .iter()
                                .map(|selection| (selection.anchor, selection.head))
                                .collect(),
                            scroll: (editor.scroll.x.into(), editor.scroll.y.into()),
                        })
                    })
                    .collect()
            })
            .collect();
        let books = self
            .notebooks
            .iter()
            .map(|(book, _)| {
                let book = book.read(cx);
                let (extension, kind) = book.reader();
                Book {
                    path: book.path.clone(),
                    extension,
                    kind,
                }
            })
            .collect();
        let pages = self
            .web_pages
            .iter()
            .map(|(page, _)| {
                let page = page.read(cx);
                Page {
                    extension: page.key.0.clone(),
                    view_type: page.model.view_type.clone(),
                    document_uri: page.model.document_uri.clone(),
                    title: page.model.title.clone(),
                    state: page.state(),
                    column: page.model.column,
                    options: page.options(),
                }
            })
            .collect();
        Session {
            version: 1,
            root: self.root(cx),
            panes,
            active: self
                .panes
                .iter()
                .map(|pane| {
                    pane.active.and_then(|active| {
                        // Untitled/output tabs are not restored, and must not shift
                        // the remembered active file to the wrong tab.
                        let editor = &pane.tabs.get(active)?.editor;
                        editor.read(cx).path(cx)?;
                        Some(
                            pane.tabs[..active]
                                .iter()
                                .filter(|tab| tab.editor.read(cx).path(cx).is_some())
                                .count(),
                        )
                    })
                })
                .collect(),
            active_pane: self.active_pane,
            books,
            pages,
            front_book: self
                .notebook_front
                .as_ref()
                .and_then(|(book, _)| self.notebooks.iter().position(|(known, _)| known == book)),
            front_page: self
                .web_front
                .as_ref()
                .and_then(|(page, _)| self.web_pages.iter().position(|(known, _)| known == page)),
        }
    }

    pub(super) fn keep_session(&self, cx: &App) -> Task<bool> {
        match &self.session_writer {
            Some(writer) if !self.restoring => writer.send(self.snapshot(cx), cx),
            _ => Task::ready(true),
        }
    }

    pub(crate) fn start_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(test) {
            return;
        }
        let path = path(&self.root(cx));
        self.session_writer = Some(Writer::new(path.clone()));
        self.restore_session(path, window, cx);
        self._subscriptions.push(cx.on_app_quit(|this, cx| {
            let saved = this.keep_session(cx);
            async move {
                saved.await;
            }
        }));
        self._session_tick = Some(cx.spawn(async move |this, cx| {
            let mut last = None;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let saved = this.update(cx, |this, cx| {
                    if this.restoring {
                        return None;
                    }
                    let snapshot = this.snapshot(cx);
                    if last.as_ref() == Some(&snapshot) {
                        return None;
                    }
                    last = Some(snapshot.clone());
                    this.session_writer
                        .as_ref()
                        .map(|writer| writer.send(snapshot, cx))
                });
                match saved {
                    Ok(Some(saved)) => {
                        if !saved.await {
                            last = None;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        }));
    }

    pub(super) fn restore_session(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.restoring = true;
        let root = self.root(cx);
        cx.spawn_in(window, async move |this, cx| {
            let read = cx.background_executor().spawn(async move {
                let saved = read(&path, &root)?;
                let files = saved.panes.iter().map(|pane| pane.iter().map(|file| {
                    std::fs::read(&file.path).ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                }).collect::<Vec<_>>()).collect::<Vec<_>>();
                Some((saved, files))
            }).await;
            let Some((saved, files)) = read else {
                this.update(cx, |this, _| this.restoring = false).ok();
                return;
            };
            // A page can only be recreated after the installed extensions
            // have been scanned. Starting its code still checks permission.
            loop {
                let loaded = cx.update(|_, cx| ExtensionStore::try_global(cx).is_none_or(|store| store.read(cx).loaded)).unwrap_or(true);
                if loaded { break; }
                cx.background_executor().timer(Duration::from_millis(25)).await;
            }
            this.update_in(cx, |this, window, cx| {
                for (pane, (_, texts)) in saved.panes.iter().zip(files).enumerate() {
                    while this.panes.len() <= pane { this.panes.push(Pane::default()); }
                    this.active_pane = pane;
                    let mut active = None;
                    for (ix, (file, text)) in saved.panes[pane].iter().zip(texts).enumerate() {
                        let Some(text) = text else { continue; };
                        let document = this.document_for_path(&file.path, cx);
                        let editor = match document {
                            Some(document) => cx.new(|cx| Editor::for_document(document, cx)),
                            None => cx.new(|cx| Editor::new(Some(file.path.clone()), &text, cx)),
                        };
                        // Reuse a CLI-opened view instead of duplicating it.
                        let known = this.panes[pane].tabs.iter().position(|tab| tab.editor.read(cx).path(cx) == Some(file.path.as_path()));
                        let editor = if let Some(known) = known { this.panes[pane].tabs[known].editor.clone() }
                            else { this.add_tab(editor.clone(), window, cx); editor };
                        editor.update(cx, |editor, cx| {
                            let ranges = file.cursors.iter().map(|(anchor, head)| *anchor..*head).collect::<Vec<_>>();
                            editor.select_ranges(&ranges, cx);
                            if file.scroll.0.is_finite() && file.scroll.1.is_finite() {
                                editor.scroll = point(px(file.scroll.0.max(0.)), px(file.scroll.1.max(0.)));
                                editor.autoscroll = false;
                            }
                        });
                        if saved.active.get(pane) == Some(&Some(ix)) { active = this.locate(&editor).map(|(_, tab)| tab); }
                    }
                    if let Some(active) = active { this.activate(pane, active, window, cx); }
                }
                this.active_pane = saved.active_pane.min(this.panes.len() - 1);
                if let Some(active) = this.panes[this.active_pane].active { this.activate(this.active_pane, active, window, cx); }
                for book in &saved.books { this.open_notebook(book.path.clone(), book.extension.clone(), book.kind.clone(), window, cx); }
                if let Some(book) = saved.front_book.and_then(|at| this.notebooks.get(at)).map(|(book, _)| book.clone()) {
                    this.show_notebook(book, this.active_pane, window, cx);
                } else { this.leave_notebook(cx); }
                cx.notify();
            }).ok();
            let mut front_page = None;
            for (at, page) in saved.pages.iter().enumerate() {
                let request = cx.update(|_, cx| ExtensionStore::try_global(cx).map(|store| store.update(cx, |store, cx| {
                    match &page.document_uri {
                        Some(uri) => store.ask_host(&page.extension,"customEditor.open",json!({"viewType":page.view_type,"uri":uri}),cx),
                        None => store.ask_host(&page.extension,"webview.restore",json!({"viewType":page.view_type,"title":page.title,"state":page.state,"column":page.column,"options":page.options}),cx),
                    }
                }))).ok().flatten();
                if let Some(request) = request {
                    match request.await {
                        Ok(id) if saved.front_page == Some(at) => {
                            front_page = id.as_str().map(|id| (page.extension.clone(), id.to_string()));
                        }
                        Ok(_) => {},
                        Err(error) => eprintln!("could not restore {}: {error}", page.title),
                    }
                }
            }
            // Host notifications and request replies take different routes
            // to the foreground. Wait for the chosen page's model before
            // selecting it, without holding an editor or a store update.
            if let Some(key) = &front_page {
                for _ in 0..100 {
                    let known = cx.update(|_, cx| ExtensionStore::try_global(cx).is_some_and(|store| store.read(cx).api.webviews.contains_key(key))).unwrap_or(false);
                    if known { break; }
                    cx.background_executor().timer(Duration::from_millis(25)).await;
                }
            }
            this.update_in(cx, |this, window, cx| {
                if let Some(key) = front_page {
                    this.open_webview(key, window, cx);
                } else if let Some(book) = saved.front_book.and_then(|at| this.notebooks.get(at)).map(|(book, _)| book.clone()) {
                    this.show_notebook(book, this.active_pane, window, cx);
                } else if let Some(active) = this.panes[this.active_pane].active {
                    this.activate(this.active_pane, active, window, cx);
                }
                this.restoring = false;
            }).ok();
        }).detach();
    }
}
