//! Text that is not saved, kept aside as it is typed.
//!
//! Whatever ends the editor without asking (a crash, the power, the key
//! that quits) would take with it what was typed and not saved. So the
//! text of every file with changes is written, a moment after it changed,
//! to a folder beside the settings, and removed from there once the file
//! is saved or its tab is closed. What is found there at the next start
//! was never saved or let go: it is put back into its tabs, as changes
//! not saved, and says so above the text.
use super::*;
use serde::{Deserialize, Serialize};
use std::sync::mpsc;

/// A text longer than this is not kept: writing it every second it
/// changes would be felt.
const KEEP_LIMIT: usize = 16 * 1024 * 1024;
/// How many texts are read back at a start, and how large a kept one may
/// be on disk to be read.
const KEPT_FILES: usize = 256;
const KEPT_BYTES: u64 = 4 * KEEP_LIMIT as u64;

#[derive(Serialize, Deserialize)]
struct Kept {
    version: u32,
    /// The file the text is of. None for a text that was never a file.
    path: Option<PathBuf>,
    text: String,
    /// Version 1 did not retain the revision of the text's original read.
    #[serde(default)]
    stamp: Option<crate::remote::Stamp>,
}

#[derive(Serialize, Deserialize)]
struct KeptBook {
    version: u32,
    path: PathBuf,
    extension: String,
    kind: String,
    #[serde(default)]
    selected: usize,
    snapshot: serde_json::Value,
}

enum Op {
    Keep {
        file: PathBuf,
        path: Option<PathBuf>,
        text: text::Rope,
        stamp: Option<crate::remote::Stamp>,
    },
    KeepBook {
        file: PathBuf,
        book: KeptBook,
        snapshot: futures::channel::oneshot::Receiver<Result<serde_json::Value, String>>,
        done: futures::channel::oneshot::Sender<bool>,
    },
    Forget(PathBuf),
    /// Everything asked before this is on disk.
    Done(futures::channel::oneshot::Sender<()>),
}

pub(super) struct Recovery {
    dir: PathBuf,
    run: u128,
    /// One thread writes, in the order it is asked: a text kept and then
    /// let go is never let go first.
    ops: mpsc::Sender<Op>,
    /// The documents whose text is kept: which state of it, and where.
    kept: HashMap<EntityId, (u64, PathBuf)>,
    restoring_books: HashMap<EntityId, u64>,
    extra_copies: HashMap<EntityId, Vec<PathBuf>>,
}

impl Recovery {
    pub(super) fn new(dir: PathBuf) -> Self {
        let (ops, asked) = mpsc::channel();
        std::thread::spawn(move || {
            for op in asked {
                match op {
                    Op::Keep {
                        file,
                        path,
                        text,
                        stamp,
                    } => {
                        let kept = Kept {
                            version: 2,
                            path,
                            text: text.to_string(),
                            stamp,
                        };
                        let written = serde_json::to_vec(&kept)
                            .map_err(|e| e.to_string())
                            .and_then(|bytes| crate::ipynb::write_atomic(&file, &bytes));
                        if let Err(error) = written {
                            crate::journal::error(format_args!(
                                "could not keep unsaved text: {error}"
                            ));
                        }
                    }
                    Op::KeepBook {
                        file,
                        mut book,
                        snapshot,
                        done,
                    } => {
                        // Waiting on this thread keeps a later Forget behind
                        // the snapshot, without waiting on an extension on UI.
                        let snapshot = futures::executor::block_on(snapshot)
                            .map_err(|_| "notebook snapshot was cancelled".to_string())
                            .and_then(|snapshot| snapshot);
                        let written = snapshot.and_then(|snapshot| {
                            book.snapshot = snapshot;
                            let bytes =
                                serde_json::to_vec(&book).map_err(|error| error.to_string())?;
                            if bytes.len() > KEEP_LIMIT {
                                // Keep the last smaller snapshot, and do not
                                // attempt this same oversized generation again.
                                return Ok(false);
                            }
                            crate::ipynb::write_atomic(&file, &bytes)?;
                            Ok(true)
                        });
                        if let Err(error) = &written {
                            crate::journal::error(format_args!(
                                "could not keep unsaved notebook: {error}"
                            ));
                        }
                        let _ = done.send(written.is_ok());
                    }
                    Op::Forget(file) => {
                        let _ = std::fs::remove_file(file);
                    }
                    Op::Done(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Self {
            dir,
            run: std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            ops,
            kept: HashMap::default(),
            restoring_books: HashMap::default(),
            extra_copies: HashMap::default(),
        }
    }

    fn adopt_book(&mut self, id: EntityId, generation: u64, file: PathBuf) {
        if let std::collections::hash_map::Entry::Vacant(entry) = self.kept.entry(id) {
            entry.insert((generation, file));
        } else {
            // An edit after restoration may already have queued a newer copy.
            // Both remain until a save or close acknowledges this notebook.
            self.extra_copies.entry(id).or_default().push(file);
        }
    }

    fn done(&self, cx: &App) -> Task<()> {
        let (done, waited) = futures::channel::oneshot::channel();
        let _ = self.ops.send(Op::Done(done));
        cx.background_executor().spawn(async move {
            let _ = waited.await;
        })
    }
}

/// Where the unsaved text of a project is kept: with the tabs of
/// projects, which the project's own watcher leaves alone.
pub(super) fn folder(root: &Path) -> PathBuf {
    settings::config_dir()
        .join("sessions")
        .join("recovery")
        .join(session::name_of(root))
}

/// What was kept in a folder, with each file it is of as it is on disk
/// now. Blocking.
fn read(dir: &Path) -> Vec<(PathBuf, Kept, Option<crate::document::Loaded>, bool)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|file| file.extension().is_some_and(|ending| ending == "json"))
        .collect();
    files.sort();
    let mut found = Vec::new();
    for file in files.into_iter().take(KEPT_FILES) {
        let small = std::fs::metadata(&file).is_ok_and(|meta| meta.len() <= KEPT_BYTES);
        let kept = small
            .then(|| std::fs::read(&file).ok())
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Kept>(&bytes).ok())
            .filter(|kept| matches!(kept.version, 1 | 2));
        // Reading is not consuming: a failed load or a window that goes
        // away before restoration must leave the only copy intact.
        if let Some(mut kept) = kept {
            if kept.version == 1 {
                kept.stamp = None;
            }
            let (on_disk, available) = match kept.path.as_deref().map(crate::document::load) {
                Some(Ok(loaded)) => (Some(loaded), true),
                Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => (None, true),
                Some(Err(_)) => (None, false),
                None => (None, true),
            };
            found.push((file, kept, on_disk, available));
        }
    }
    found
}

fn read_books(dir: &Path) -> Vec<(PathBuf, KeptBook)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".book.json"))
        })
        .collect::<Vec<_>>();
    files.sort();
    files
        .into_iter()
        .take(KEPT_FILES)
        .filter_map(|file| {
            let metadata = std::fs::symlink_metadata(&file).ok()?;
            if !metadata.is_file() || metadata.len() > KEEP_LIMIT as u64 {
                return None;
            }
            let bytes = std::fs::read(&file).ok()?;
            let book: KeptBook = serde_json::from_slice(&bytes).ok()?;
            if book.version != 1
                || !book.path.is_absolute()
                || book.kind.is_empty()
                || (!book.extension.is_empty() && book.snapshot["type"] != book.kind)
                || (book.extension.is_empty() && book.kind != "jupyter-notebook")
            {
                return None;
            }
            Some((file, book))
        })
        .collect()
}

impl Workspace {
    /// Keeps the text of every document that has changes, where it is not
    /// kept as it is now, and lets go of what was kept of the ones that
    /// have none or are closed. Resolves once that is on disk.
    pub(super) fn keep_unsaved(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let documents = self.documents(cx);
        let Some(recovery) = self.recovery.as_mut() else {
            return Task::ready(());
        };
        let mut changed = HashSet::new();
        for document in documents {
            let id = document.entity_id();
            let doc = document.read(cx);
            if !doc.is_dirty() {
                continue;
            }
            if doc.is_read_only() || doc.text().len() > KEEP_LIMIT {
                // A document that outgrew the limit still has its last
                // recoverable copy; the limit must not discard it.
                if recovery.kept.contains_key(&id) {
                    changed.insert(id);
                }
                continue;
            }
            changed.insert(id);
            let known = recovery.kept.get(&id);
            if known.is_some_and(|(version, _)| *version == doc.version()) {
                continue;
            }
            let file = known.map(|(_, file)| file.clone()).unwrap_or_else(|| {
                let name = match doc.path() {
                    Some(path) => session::name_of(path),
                    None => format!("untitled-{}-{}", std::process::id(), id.as_u64()),
                };
                // A not-yet-restored copy for this path may still exist,
                // including one whose target could not be read.
                recovery.dir.join(format!(
                    "{name}-{}-{}-{}.json",
                    std::process::id(),
                    recovery.run,
                    id.as_u64()
                ))
            });
            let (path, text, stamp) = doc.recovery_snapshot();
            let keep = Op::Keep {
                file: file.clone(),
                path,
                text,
                stamp,
            };
            let _ = recovery.ops.send(keep);
            recovery.kept.insert(id, (doc.version(), file));
        }
        for (notebook, _) in &self.notebooks {
            let id = notebook.entity_id();
            let book = notebook.read(cx);
            if recovery.restoring_books.get(&id).is_some_and(|generation| {
                *generation == book.version() || book.recovered_generation() == Some(book.version())
            }) {
                // The original copy has not been adopted yet. Do not create
                // a second one between its ready event and adoption.
                changed.insert(id);
                continue;
            }
            if !book.is_dirty() {
                continue;
            }
            changed.insert(id);
            let version = book.version();
            let known = recovery.kept.get(&id);
            if known.is_some_and(|(kept, _)| *kept == version) {
                continue;
            }
            let file = known.map(|(_, file)| file.clone()).unwrap_or_else(|| {
                recovery.dir.join(format!(
                    "notebook-{}-{}-{}-{}.book.json",
                    session::name_of(&book.path),
                    std::process::id(),
                    recovery.run,
                    id.as_u64()
                ))
            });
            let (extension, kind) = book.reader();
            let kept = KeptBook {
                version: 1,
                path: book.path.clone(),
                extension,
                kind,
                selected: book.selected(),
                snapshot: serde_json::Value::Null,
            };
            let Some(snapshot) = notebook.update(cx, |book, cx| book.recovery_snapshot(cx)) else {
                continue;
            };
            let (send, snapshot_rx) = futures::channel::oneshot::channel();
            let (done, acknowledged) = futures::channel::oneshot::channel();
            let _ = recovery.ops.send(Op::KeepBook {
                file: file.clone(),
                book: kept,
                snapshot: snapshot_rx,
                done,
            });
            recovery.kept.insert(id, (version, file));
            cx.spawn(async move |this, cx| {
                let _ = send.send(snapshot.await);
                let success = acknowledged.await.unwrap_or(false);
                if !success {
                    this.update(cx, |this, _| {
                        if let Some(recovery) = this.recovery.as_mut()
                            && let Some((kept, _)) = recovery.kept.get_mut(&id)
                            && *kept == version
                        {
                            *kept = version.wrapping_sub(1);
                        }
                    })
                    .ok();
                }
            })
            .detach();
        }
        // Saved, local undo back to disk, or explicitly closed. A remote
        // recovery stays dirty until it is saved or closed with an answer.
        recovery.kept.retain(|id, (_, file)| {
            let stays = changed.contains(id);
            if !stays {
                let _ = recovery.ops.send(Op::Forget(file.clone()));
            }
            stays
        });
        recovery.extra_copies.retain(|id, files| {
            if changed.contains(id) {
                return true;
            }
            for file in files {
                let _ = recovery.ops.send(Op::Forget(file.clone()));
            }
            false
        });
        recovery.done(cx)
    }

    /// Lets go of everything kept: the window was closed, and what was
    /// not saved was asked about.
    pub(super) fn forget_unsaved(&mut self, cx: &App) -> Task<()> {
        let Some(recovery) = self.recovery.as_mut() else {
            return Task::ready(());
        };
        for (_, (_, file)) in recovery.kept.drain() {
            let _ = recovery.ops.send(Op::Forget(file));
        }
        for (_, files) in recovery.extra_copies.drain() {
            for file in files {
                let _ = recovery.ops.send(Op::Forget(file));
            }
        }
        recovery.done(cx)
    }

    /// Puts back the text that was kept and never saved or let go, once
    /// the tabs of the project are back: into the tab of its file where
    /// that is open, into a new one where it is not.
    pub(super) fn recover_unsaved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.recovery.as_ref().map(|recovery| recovery.dir.clone()) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            while this.update(cx, |this, _| this.restoring).unwrap_or(false) {
                cx.background_executor()
                    .timer(Duration::from_millis(25))
                    .await;
            }
            let (found, books) = cx
                .background_executor()
                .spawn(async move { (read(&dir), read_books(&dir)) })
                .await;
            this.update_in(cx, |this, window, cx| {
                for (file, kept, on_disk, available) in found {
                    if available {
                        this.recover_one(file, kept, on_disk, window, cx);
                    }
                }
            })
            .ok();
            for (file, kept) in books {
                let KeptBook {
                    path,
                    extension,
                    kind,
                    selected,
                    snapshot,
                    ..
                } = kept;
                let Ok(restoring) = this.update_in(cx, |this, window, cx| {
                    let known = this
                        .notebooks
                        .iter()
                        .find(|(book, _)| book.read(cx).path == path)
                        .map(|(book, _)| book.clone());
                    if known.as_ref().is_some_and(|book| {
                        let book = book.read(cx);
                        book.is_dirty() || !book.is(&path, &extension, &kind)
                    }) {
                        return None;
                    }
                    let book = match known {
                        Some(book) => book,
                        None => {
                            this.open_notebook(path, extension, kind, window, cx);
                            this.notebook_front.as_ref()?.0.clone()
                        }
                    };
                    if let Some(recovery) = this.recovery.as_mut() {
                        recovery
                            .restoring_books
                            .insert(book.entity_id(), book.read(cx).version());
                    }
                    let restoring =
                        book.update(cx, |book, cx| book.restore(snapshot, selected, cx));
                    Some((book, restoring))
                }) else {
                    continue;
                };
                let Some((book, restoring)) = restoring else {
                    continue;
                };
                let restored = restoring.await;
                this.update_in(cx, |this, window, cx| {
                    if let Some(recovery) = this.recovery.as_mut() {
                        recovery.restoring_books.remove(&book.entity_id());
                    }
                    if !restored || !this.notebooks.iter().any(|(open, _)| *open == book) {
                        return;
                    }
                    if let Some(recovery) = this.recovery.as_mut() {
                        let generation = book
                            .read(cx)
                            .recovered_generation()
                            .unwrap_or_else(|| book.read(cx).version());
                        recovery.adopt_book(book.entity_id(), generation, file);
                    }
                    if this
                        .notebook_front
                        .as_ref()
                        .is_some_and(|(front, _)| *front == book)
                    {
                        book.read(cx).focus(window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn recover_one(
        &mut self,
        file: PathBuf,
        kept: Kept,
        on_disk: Option<crate::document::Loaded>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if (kept.path.as_deref().is_some_and(crate::remote::is_remote) && on_disk.is_none())
            || on_disk
                .as_ref()
                .is_some_and(|file| file.read_only.is_some())
        {
            return;
        }
        let open = kept
            .path
            .as_deref()
            .and_then(|path| self.document_for_path(path, cx));
        // Typing while the background read ran must not be overwritten.
        if open.as_ref().is_some_and(|document| {
            let document = document.read(cx);
            document.is_dirty() || document.is_read_only()
        }) {
            return;
        }
        let document = match (open, &kept.path, on_disk) {
            (Some(document), _, _) => document,
            (None, Some(path), Some(file)) => {
                self.add_loaded(path.clone(), &file, None, window, cx);
                let Some(editor) = self.active_editor() else {
                    return;
                };
                editor.read(cx).document().clone()
            }
            // Its file is gone, or it never had one: the text alone.
            (None, path, _) => {
                self.add_editor(path.clone(), "", None, window, cx);
                let Some(editor) = self.active_editor() else {
                    return;
                };
                editor.read(cx).document().clone()
            }
        };
        let restored = document.update(cx, |document, cx| {
            document.recover(&kept.text, kept.stamp, cx)
        });
        let Some(recovery) = self.recovery.as_mut() else {
            return;
        };
        if restored {
            // Adopt the existing copy before another keep/quit. Force one
            // rewrite so legacy entries migrate through the same writer.
            let version = document.read(cx).version().wrapping_sub(1);
            recovery.kept.insert(document.entity_id(), (version, file));
        } else {
            // An editable local document already contains the kept text.
            let _ = recovery.ops.send(Op::Forget(file));
        }
    }

    /// Begins keeping unsaved text for this window's project, and puts
    /// back what an earlier run of the editor left.
    pub(super) fn start_recovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A benchmark types into a real file and quits. What it typed is
        // nobody's unsaved work: kept, it would be put into that file's
        // tab the next time its project is opened.
        let benchmark = |(name, _): (std::ffi::OsString, std::ffi::OsString)| {
            name.to_string_lossy().starts_with("SOLDER_BENCH")
        };
        if std::env::vars_os().any(benchmark) {
            return;
        }
        self.recovery = Some(Recovery::new(folder(&self.root(cx))));
        self.recover_unsaved(window, cx);
        // An acknowledged quit has already flushed or discarded its
        // recovery. Other app exits still keep the last of what was typed.
        self._subscriptions.push(cx.on_app_quit(|this, cx| {
            if quit::confirmed(cx) || this.close_confirmed {
                Task::ready(())
            } else {
                this.keep_unsaved(cx)
            }
        }));
        self._recovery_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(kept) = this.update(cx, |this, cx| {
                    match this.restoring || this.close_preparing || quit::pause_recovery(cx) {
                        true => Task::ready(()),
                        false => this.keep_unsaved(cx),
                    }
                }) else {
                    break;
                };
                kept.await;
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    async fn notebook_snapshots_precede_forget_and_adoption_preserves_newer_copies(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let dir = db::testing::dir("recovery-notebook-writer");
        let document = cx.new(|cx| Document::new(None, "", cx));
        let id = document.entity_id();
        let mut recovery = Recovery::new(dir.clone());
        let old = dir.join("older.book.json");
        let newer = dir.join("newer.book.json");
        std::fs::write(&old, "original backup").unwrap();
        recovery.kept.insert(id, (2, newer.clone()));
        recovery.adopt_book(id, 1, old.clone());
        assert_eq!(recovery.kept[&id], (2, newer.clone()));
        assert_eq!(
            recovery.extra_copies[&id].as_slice(),
            std::slice::from_ref(&old)
        );
        assert!(old.exists());
        let (send, snapshot) = futures::channel::oneshot::channel();
        let (done, acknowledged) = futures::channel::oneshot::channel();
        recovery
            .ops
            .send(Op::KeepBook {
                file: newer.clone(),
                book: KeptBook {
                    version: 1,
                    path: dir.join("book.ipynb"),
                    extension: String::new(),
                    kind: "jupyter-notebook".into(),
                    selected: 0,
                    snapshot: serde_json::Value::Null,
                },
                snapshot,
                done,
            })
            .unwrap();
        recovery.ops.send(Op::Forget(newer.clone())).unwrap();
        let (done, mut finished) = futures::channel::oneshot::channel();
        recovery.ops.send(Op::Done(done)).unwrap();
        assert!(finished.try_recv().unwrap().is_none());
        send.send(Ok(serde_json::json!({"content":"newer"})))
            .unwrap();
        assert!(acknowledged.await.unwrap());
        cx.update(|cx| recovery.done(cx)).await;
        assert!(!newer.exists());
        assert!(old.exists());
        recovery.ops.send(Op::Forget(old.clone())).unwrap();
        cx.update(|cx| recovery.done(cx)).await;
        assert!(!old.exists());
    }

    #[test]
    fn recovery_versions_are_read_without_consuming_any_copy() {
        let dir = db::testing::dir("recovery-versions");
        let stamp = crate::remote::Stamp {
            length: 3,
            mtime: 42,
            hash: Some("original".into()),
        };
        let legacy = serde_json::json!({"version":1,"path":null,"text":"legacy"});
        let current = serde_json::json!({"version":2,"path":null,"text":"current","stamp":stamp});
        for (name, value) in [
            ("legacy", legacy),
            ("current", current),
            (
                "future",
                serde_json::json!({"version":3,"path":null,"text":"future"}),
            ),
        ] {
            std::fs::write(dir.join(format!("{name}.json")), value.to_string()).unwrap();
        }
        std::fs::write(dir.join("broken.json"), b"{unfinished").unwrap();
        std::fs::File::create(dir.join("large.json"))
            .unwrap()
            .set_len(KEPT_BYTES + 1)
            .unwrap();
        let found = read(&dir);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].1.text, "current");
        assert_eq!(found[0].1.stamp, Some(stamp));
        assert_eq!(found[1].1.text, "legacy");
        assert_eq!(found[1].1.stamp, None);
        assert!(found.iter().all(|(_, _, loaded, _)| loaded.is_none()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 5);
        assert_eq!(read(&dir).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn unavailable_remote_recovery_is_unchanged_and_can_be_retried() {
        let dir = db::testing::dir("recovery-unavailable")
            .canonicalize()
            .unwrap();
        let source = dir.join("server");
        let cache = dir.join("cache");
        let copies = dir.join("copies");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&copies).unwrap();
        let target = source.join("file.txt");
        let path = cache.join("file.txt");
        std::fs::write(&target, b"original").unwrap();
        let mount = crate::remote::fixture(&source, &cache);
        let stamp = crate::document::load(&path).unwrap().stamp;
        let file = copies.join("work.json");
        let bytes = serde_json::to_vec(&Kept {
            version: 2,
            path: Some(path.clone()),
            text: "unsaved work".into(),
            stamp: stamp.clone(),
        })
        .unwrap();
        std::fs::write(&file, &bytes).unwrap();
        drop(mount);
        let found = read(&copies);
        assert_eq!(found.len(), 1);
        assert!(found[0].2.is_none());
        assert_eq!(std::fs::read(&file).unwrap(), bytes);

        let _mount = crate::remote::fixture(&source, &cache);
        std::fs::remove_file(&target).unwrap();
        assert!(read(&copies)[0].2.is_none());
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        std::fs::write(&target, b"\0\x01\x02").unwrap();
        assert!(read(&copies)[0].2.as_ref().unwrap().read_only.is_some());
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        std::fs::write(&target, b"new target").unwrap();
        let found = read(&copies);
        assert_eq!(found[0].1.stamp, stamp);
        assert_eq!(found[0].2.as_ref().unwrap().text, "new target");
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn writer_and_restart_keep_the_original_remote_revision(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let dir = db::testing::dir("recovery-restart").canonicalize().unwrap();
        let source = dir.join("server");
        let cache = dir.join("cache");
        std::fs::create_dir(&source).unwrap();
        let target = source.join("file.txt");
        let path = cache.join("file.txt");
        std::fs::write(&target, [0xff, 0xfe, b'A', 0]).unwrap();
        let mount = crate::remote::fixture(&source, &cache);
        let loaded = crate::document::load(&path).unwrap();
        let original = loaded.stamp.clone().unwrap();
        let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
        document.update(cx, |doc, cx| {
            doc.apply_edits(vec![(0..1, "U".into())], None, cx)
        });
        let (recovered_path, text, stamp) =
            document.read_with(cx, |doc, _| doc.recovery_snapshot());
        let recovery = Recovery::new(dir.join("copies"));
        let file = recovery.dir.join("work.json");
        recovery
            .ops
            .send(Op::Keep {
                file: file.clone(),
                path: recovered_path,
                text,
                stamp,
            })
            .unwrap();
        cx.update(|cx| recovery.done(cx)).await;
        let written: Kept = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(written.version, 2);
        assert_eq!(written.stamp, Some(original.clone()));
        drop(document);
        drop(recovery);
        drop(mount);

        // Same length and mtime: the persisted hash still rejects the
        // change made while Solder was not running.
        std::fs::write(&target, [0xff, 0xfe, b'B', 0]).unwrap();
        std::fs::File::open(&target)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_nanos(original.mtime))
            .unwrap();
        let _mount = crate::remote::fixture(&source, &cache);
        let (copy, kept, loaded, _) = read(file.parent().unwrap()).pop().unwrap();
        assert_eq!(copy, file);
        assert_eq!(kept.stamp, Some(original));
        let loaded = loaded.unwrap();
        let restored = cx.new(|cx| Document::open(path, &loaded, cx));
        assert!(restored.update(cx, |doc, cx| doc.recover(&kept.text, kept.stamp, cx)));
        assert!(!restored.update(cx, |doc, cx| doc.save(cx)).await);
        assert_eq!(std::fs::read(&target).unwrap(), [0xff, 0xfe, b'B', 0]);
        assert!(restored.read_with(cx, |doc, _| doc.is_dirty()));
        assert!(file.exists());
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn failed_save_as_after_undo_preserves_the_original_recovery(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let dir = db::testing::dir("recovery-failed-save-as")
            .canonicalize()
            .unwrap();
        let source = dir.join("server");
        let cache = dir.join("cache");
        std::fs::create_dir(&source).unwrap();
        // A file used as a parent produces a real target IO failure.
        std::fs::write(source.join("blocker"), b"not a directory").unwrap();
        let _mount = crate::remote::fixture(&source, &cache);
        let recovery = Recovery::new(dir.join("copies"));

        for legacy in [false, true] {
            let name = if legacy { "legacy" } else { "versioned" };
            let target = source.join(format!("{name}.txt"));
            let path = cache.join(format!("{name}.txt"));
            std::fs::write(&target, b"before").unwrap();
            let stamp = crate::document::load(&path)
                .unwrap()
                .stamp
                .filter(|_| !legacy);
            let baseline = "today 🐟";
            std::fs::write(&target, baseline.as_bytes()).unwrap();
            let loaded = crate::document::load(&path).unwrap();
            let document = cx.new(|cx| Document::open(path.clone(), &loaded, cx));
            document.update(cx, |doc, cx| doc.recover("unsaved work", stamp.clone(), cx));
            let copy = recovery.dir.join(format!("{name}.json"));
            // The same snapshot and ordered writer that keep_unsaved uses.
            let keep = |cx: &mut gpui::TestAppContext| {
                let (path, text, stamp) = document.read_with(cx, |doc, _| doc.recovery_snapshot());
                recovery
                    .ops
                    .send(Op::Keep {
                        file: copy.clone(),
                        path,
                        text,
                        stamp,
                    })
                    .unwrap();
                cx.update(|cx| recovery.done(cx))
            };
            let assert_original = || {
                let kept: Kept = serde_json::from_slice(&std::fs::read(&copy).unwrap()).unwrap();
                assert_eq!(kept.path.as_ref(), Some(&path));
                assert_eq!(kept.text, "unsaved work");
                assert_eq!(kept.stamp, stamp);
            };
            keep(cx).await;
            assert_original();

            let origin = document.entity_id();
            document.update(cx, |doc, cx| doc.undo(origin, cx)).unwrap();
            let destination = cache.join(format!("{name}-new.txt"));
            document.update(cx, |doc, cx| {
                doc.set_path(destination.clone(), cx);
                doc.set_encoding(text::encoding::Encoding::Windows1252, cx);
            });
            assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
            assert!(document.read_with(cx, |doc, _| {
                doc.notice().unwrap().contains("cannot be written")
            }));
            assert!(!source.join(format!("{name}-new.txt")).exists());
            assert!(document.read_with(cx, |doc, _| doc.is_dirty()));
            keep(cx).await;
            assert_original();

            document.update(cx, |doc, cx| {
                doc.set_encoding(text::encoding::Encoding::Utf8, cx);
                doc.set_path(cache.join("blocker/new.txt"), cx);
            });
            assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
            assert!(document.read_with(cx, |doc, _| {
                doc.notice().unwrap().starts_with("Not saved:")
            }));
            keep(cx).await;
            assert_original();

            // Returning to the source still uses its original revision,
            // even after a destination reset the document's save stamp.
            document.update(cx, |doc, cx| doc.set_path(path.clone(), cx));
            assert!(!document.update(cx, |doc, cx| doc.save(cx)).await);
            keep(cx).await;
            assert_original();

            // The original stamp (including a legacy unknown one) must
            // not be used as the destination's expected revision.
            document.update(cx, |doc, cx| doc.set_path(destination.clone(), cx));
            assert!(document.update(cx, |doc, cx| doc.save(cx)).await);
            assert_eq!(
                std::fs::read(source.join(format!("{name}-new.txt"))).unwrap(),
                baseline.as_bytes()
            );
            assert_eq!(std::fs::read(&target).unwrap(), baseline.as_bytes());
            assert!(!document.read_with(cx, |doc, _| doc.is_dirty()));
            let (path, text, stamp) = document.read_with(cx, |doc, _| doc.recovery_snapshot());
            assert_eq!(path, Some(destination));
            assert_eq!(text.to_string(), baseline);
            assert!(stamp.is_some());
        }
    }
}
