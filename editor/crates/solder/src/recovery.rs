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
}

enum Op {
    Keep {
        file: PathBuf,
        path: Option<PathBuf>,
        text: text::Rope,
    },
    Forget(PathBuf),
    /// Everything asked before this is on disk.
    Done(futures::channel::oneshot::Sender<()>),
}

pub(super) struct Recovery {
    dir: PathBuf,
    /// One thread writes, in the order it is asked: a text kept and then
    /// let go is never let go first.
    ops: mpsc::Sender<Op>,
    /// The documents whose text is kept: which state of it, and where.
    kept: HashMap<EntityId, (u64, PathBuf)>,
}

impl Recovery {
    pub(super) fn new(dir: PathBuf) -> Self {
        let (ops, asked) = mpsc::channel();
        std::thread::spawn(move || {
            for op in asked {
                match op {
                    Op::Keep { file, path, text } => {
                        let kept = Kept {
                            version: 1,
                            path,
                            text: text.to_string(),
                        };
                        let written = serde_json::to_vec(&kept)
                            .map_err(|e| e.to_string())
                            .and_then(|bytes| crate::ipynb::write_atomic(&file, &bytes));
                        if let Err(error) = written {
                            eprintln!("could not keep unsaved text: {error}");
                        }
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
            ops,
            kept: HashMap::default(),
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
fn read(dir: &Path) -> Vec<(Kept, Option<crate::document::Loaded>)> {
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
            .filter(|kept| kept.version == 1);
        // Read or not, it is done with: what is put back is kept anew.
        let _ = std::fs::remove_file(&file);
        if let Some(kept) = kept {
            let on_disk = kept
                .path
                .as_deref()
                .and_then(|path| crate::document::load(path).ok());
            found.push((kept, on_disk));
        }
    }
    found
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
            if !doc.is_dirty() || doc.is_read_only() || doc.text().len() > KEEP_LIMIT {
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
                recovery.dir.join(format!("{name}.json"))
            });
            let keep = Op::Keep {
                file: file.clone(),
                path: doc.path().map(Path::to_path_buf),
                text: doc.text().rope().clone(),
            };
            let _ = recovery.ops.send(keep);
            recovery.kept.insert(id, (doc.version(), file));
        }
        // Saved, undone back to what is on disk, or closed.
        recovery.kept.retain(|id, (_, file)| {
            let stays = changed.contains(id);
            if !stays {
                let _ = recovery.ops.send(Op::Forget(file.clone()));
            }
            stays
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
            let found = cx
                .background_executor()
                .spawn(async move { read(&dir) })
                .await;
            this.update_in(cx, |this, window, cx| {
                for (kept, on_disk) in found {
                    this.recover_one(kept, on_disk, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn recover_one(
        &mut self,
        kept: Kept,
        on_disk: Option<crate::document::Loaded>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = kept
            .path
            .as_deref()
            .and_then(|path| self.document_for_path(path, cx));
        let document = match (open, &kept.path, on_disk) {
            (Some(document), _, _) => document,
            // A file that can no longer be edited is left as it is.
            (None, Some(_), Some(file)) if file.read_only.is_some() => return,
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
        document.update(cx, |document, cx| document.recover(&kept.text, cx));
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
        // Asked to quit, the last of what was typed is kept first: the
        // key that quits asks nothing.
        self._subscriptions
            .push(cx.on_app_quit(|this, cx| this.keep_unsaved(cx)));
        self._recovery_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(kept) = this.update(cx, |this, cx| match this.restoring {
                    true => Task::ready(()),
                    false => this.keep_unsaved(cx),
                }) else {
                    break;
                };
                kept.await;
            }
        }));
    }
}
