//! A quit is one decision for all project windows. None closes until saves
//! and persistence finish, and changes made while waiting are checked again.
use super::*;
use futures::future::Shared;
use gpui::{Global, WindowHandle, WindowId};

#[derive(Default, PartialEq)]
enum Phase {
    #[default]
    Idle,
    Reviewing,
    Finalizing,
    Confirmed,
}
#[derive(Default)]
struct State(Phase);
impl Global for State {}

pub(super) fn busy(cx: &App) -> bool {
    cx.try_global::<State>()
        .is_some_and(|state| state.0 != Phase::Idle)
}
pub(super) fn confirmed(cx: &App) -> bool {
    cx.try_global::<State>()
        .is_some_and(|state| state.0 == Phase::Confirmed)
}
pub(super) fn pause_recovery(cx: &App) -> bool {
    cx.try_global::<State>()
        .is_some_and(|state| matches!(state.0, Phase::Finalizing | Phase::Confirmed))
}
fn phase(phase: Phase, cx: &mut App) {
    cx.set_global(State(phase));
}

#[derive(PartialEq)]
enum Revision {
    File(u64, Option<PathBuf>),
    Book(u64, PathBuf),
}
#[derive(Default)]
pub(super) struct Changes {
    revisions: HashMap<EntityId, Revision>,
    titles: Vec<String>,
}
impl Changes {
    pub(super) fn of(workspace: &Workspace, cx: &App) -> Self {
        let mut changes = Self::default();
        changes.add(workspace, cx);
        changes
    }
    fn add(&mut self, workspace: &Workspace, cx: &App) {
        for document in workspace.documents(cx) {
            let doc = document.read(cx);
            if doc.is_dirty()
                && self
                    .revisions
                    .insert(
                        document.entity_id(),
                        Revision::File(doc.version(), doc.path().map(Path::to_path_buf)),
                    )
                    .is_none()
            {
                self.titles.push(doc.title());
            }
        }
        for (notebook, _) in &workspace.notebooks {
            let book = notebook.read(cx);
            if book.is_dirty()
                && self
                    .revisions
                    .insert(
                        notebook.entity_id(),
                        Revision::Book(book.version(), book.path.clone()),
                    )
                    .is_none()
            {
                self.titles.push(book.title());
            }
        }
    }
    pub(super) fn copies_to_save(&self, cx: &App) -> HashMap<EntityId, (PathBuf, bool)> {
        Snapshot::capture(cx)
            .copies
            .into_iter()
            .map(|(id, (path, dirty))| (id, (path, dirty && self.revisions.contains_key(&id))))
            .collect()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.revisions.is_empty()
    }
    pub(super) fn permits(&self, current: &Self) -> bool {
        current
            .revisions
            .iter()
            .all(|(id, revision)| self.revisions.get(id) == Some(revision))
    }
    pub(super) fn message(&self) -> String {
        match self.titles.as_slice() {
            [one] => tf("Save changes to {}?", &[one]),
            _ => tf("Save changes to {} files?", &[&self.revisions.len()]),
        }
    }
}
struct Snapshot {
    windows: Vec<WindowHandle<Workspace>>,
    changes: Changes,
    copies: HashMap<EntityId, (PathBuf, bool)>,
}
impl Snapshot {
    fn capture(cx: &App) -> Self {
        let windows: Vec<_> = cx
            .windows()
            .into_iter()
            .filter_map(|window| window.downcast::<Workspace>())
            .collect();
        let mut changes = Changes::default();
        let mut copies = HashMap::new();
        for window in &windows {
            if let Ok(workspace) = window.read(cx) {
                changes.add(workspace, cx);
                for document in workspace.documents(cx) {
                    let doc = document.read(cx);
                    if let Some(path) = doc.path() {
                        copies.insert(document.entity_id(), (path.to_path_buf(), doc.is_dirty()));
                    }
                }
                for (notebook, _) in &workspace.notebooks {
                    let book = notebook.read(cx);
                    copies.insert(notebook.entity_id(), (book.path.clone(), book.is_dirty()));
                }
            }
        }
        Self {
            windows,
            changes,
            copies,
        }
    }
    fn same_copies(&self, other: &Self) -> bool {
        self.copies.len() == other.copies.len()
            && self.copies.iter().all(|(id, (path, _))| {
                other
                    .copies
                    .get(id)
                    .is_some_and(|(current, _)| current == path)
            })
    }
    fn pending_saves(&self, cx: &App) -> Vec<Shared<Task<bool>>> {
        self.windows
            .iter()
            .filter_map(|window| window.read(cx).ok())
            .flat_map(|workspace| workspace.pending_saves(cx))
            .collect()
    }
    fn window_ids(&self) -> HashSet<WindowId> {
        self.windows
            .iter()
            .map(|window| window.window_id())
            .collect()
    }
    fn host(&self, cx: &App) -> Option<WindowHandle<Workspace>> {
        cx.active_window()
            .and_then(|window| window.downcast::<Workspace>())
            .or_else(|| self.windows.first().copied())
    }
}

pub(super) fn copies_with_target(
    document: EntityId,
    target: &Path,
    cx: &App,
) -> HashMap<EntityId, (PathBuf, bool)> {
    let mut copies = Snapshot::capture(cx).copies;
    copies.insert(document, (target.to_path_buf(), true));
    copies
}

pub(super) fn request(cx: &mut App) {
    if busy(cx) {
        return;
    }
    let initial = Snapshot::capture(cx);
    // A window's close already has a decision or save pending. Do not stack
    // another prompt over it or race its recovery deletion.
    if initial
        .windows
        .iter()
        .any(|window| window.read(cx).is_ok_and(|workspace| workspace.closing))
    {
        return;
    }
    phase(Phase::Reviewing, cx);
    cx.spawn(async move |cx| {
        let mut snapshot = initial;
        loop {
            // Manual saves can be pending even after an undo returned to clean
            // text. Finish those writes before asking about the resulting state.
            loop {
                let Ok(pending) = cx.update(|cx| Snapshot::capture(cx).pending_saves(cx)) else {
                    return;
                };
                if pending.is_empty() {
                    break;
                }
                for pending in pending {
                    pending.await;
                }
                let Ok(current) = cx.update(|cx| Snapshot::capture(cx)) else {
                    return;
                };
                snapshot = current;
            }
            let discard = if snapshot.changes.is_empty() {
                false
            } else {
                let asked = cx
                    .update(|cx| {
                        let host = snapshot.host(cx)?;
                        host.update(cx, |_, window, cx| {
                            window.activate_window();
                            window.prompt(
                                PromptLevel::Warning,
                                &snapshot.changes.message(),
                                Some(t("Your changes will be lost if you don't save them.")),
                                &[t("Save All"), t("Don't Save"), t("Cancel")],
                                cx,
                            )
                        })
                        .ok()
                    })
                    .ok()
                    .flatten();
                let Some(asked) = asked else {
                    break;
                };
                match asked.await.ok() {
                    Some(0) => {
                        let paths = snapshot.copies.values().cloned().collect();
                        let duplicate = cx
                            .background_executor()
                            .spawn(async move { duplicate_file(paths) })
                            .await;
                        if let Some(path) = duplicate {
                            let message = tf("Multiple open copies of {}", &[&path.display()]);
                            let shown = cx
                                .update(|cx| {
                                    let host = snapshot.host(cx)?;
                                    host.update(cx, |_, window, cx| {
                                        window.prompt(
                                            PromptLevel::Warning,
                                            &message,
                                            Some(t("Close the other copies before saving.")),
                                            &[t("OK")],
                                            cx,
                                        )
                                    })
                                    .ok()
                                })
                                .ok()
                                .flatten();
                            if let Some(shown) = shown {
                                shown.await.ok();
                            }
                            break;
                        }
                        let Ok(current) = cx.update(|cx| Snapshot::capture(cx)) else {
                            break;
                        };
                        if !snapshot.changes.permits(&current.changes)
                            || !snapshot.same_copies(&current)
                            || current.window_ids() != snapshot.window_ids()
                        {
                            snapshot = current;
                            continue;
                        }
                        let mut saved = true;
                        let mut changed = false;
                        for window in snapshot.windows.clone() {
                            let Ok(current) = cx.update(|cx| Snapshot::capture(cx)) else {
                                return;
                            };
                            if !snapshot.changes.permits(&current.changes)
                                || !snapshot.same_copies(&current)
                                || current.window_ids() != snapshot.window_ids()
                            {
                                snapshot = current;
                                changed = true;
                                break;
                            }
                            let tasks = window
                                .update(cx, |workspace, window, cx| {
                                    workspace.save_dirty(window, cx)
                                })
                                .unwrap_or_default();
                            for task in tasks {
                                saved &= task.await;
                            }
                        }
                        if changed {
                            continue;
                        }
                        if !saved {
                            break;
                        }
                        let Ok(current) = cx.update(|cx| Snapshot::capture(cx)) else {
                            break;
                        };
                        if !current.changes.is_empty()
                            || current.window_ids() != snapshot.window_ids()
                        {
                            snapshot = current;
                            continue;
                        }
                        snapshot = current;
                        false
                    }
                    Some(1) => {
                        let Ok(current) = cx.update(|cx| Snapshot::capture(cx)) else {
                            break;
                        };
                        if !snapshot.changes.permits(&current.changes)
                            || current.window_ids() != snapshot.window_ids()
                        {
                            snapshot = current;
                            continue;
                        }
                        snapshot = current;
                        true
                    }
                    _ => break,
                }
            };
            // Stop periodic recovery writes until this decision is either
            // cancelled by a new edit or completed. Explicit flushes still run.
            if cx.update(|cx| phase(Phase::Finalizing, cx)).is_err() {
                return;
            }
            let mut kept = true;
            for window in &snapshot.windows {
                match window.update(cx, |workspace, _, cx| {
                    (workspace.keep_session(cx), workspace.keep_unsaved(cx))
                }) {
                    Ok((session, recovery)) => {
                        kept &= session.await;
                        recovery.await;
                    }
                    Err(_) => kept = false,
                }
            }
            if !kept {
                break;
            }
            for window in &snapshot.windows {
                if let Ok(forgotten) =
                    window.update(cx, |workspace, _, cx| workspace.forget_unsaved(cx))
                {
                    forgotten.await;
                }
            }
            if let Ok(flushed) = cx.update(|cx| crate::windows::flush(cx)) {
                flushed.await;
            }
            let finish = cx.update(|cx| {
                let current = Snapshot::capture(cx);
                let unchanged = current.pending_saves(cx).is_empty()
                    && current.window_ids() == snapshot.window_ids()
                    && if discard {
                        snapshot.changes.permits(&current.changes)
                    } else {
                        current.changes.is_empty()
                    };
                if unchanged {
                    phase(Phase::Confirmed, cx);
                    crate::windows::quitting(cx);
                    cx.quit();
                    None
                } else {
                    phase(Phase::Reviewing, cx);
                    Some(current)
                }
            });
            match finish {
                Ok(None) | Err(_) => return,
                Ok(Some(current)) => {
                    // The deletion was for an older approved state. Put the
                    // current work back before asking about these new changes.
                    for window in &current.windows {
                        if let Ok(kept) =
                            window.update(cx, |workspace, _, cx| workspace.keep_unsaved(cx))
                        {
                            kept.await;
                        }
                    }
                    snapshot = current;
                }
            }
        }
        cx.update(|cx| phase(Phase::Idle, cx)).ok();
    })
    .detach();
}

/// Different windows can own independent buffers of the same local file.
/// Resolve aliases off the UI thread before Save All can overwrite one copy.
pub(super) fn duplicate_file(paths: Vec<(PathBuf, bool)>) -> Option<PathBuf> {
    let mut names = HashMap::new();
    #[cfg(unix)]
    let mut files = HashMap::new();
    for (path, dirty) in paths {
        // Remote cache metadata is not the target's identity. Equal remote
        // paths still collide; other target aliases rely on revision checks.
        let remote = crate::remote::is_remote(&path);
        let resolved = if remote {
            path.clone()
        } else {
            path.canonicalize().unwrap_or_else(|_| path.clone())
        };
        if let Some(previous) = names.insert(resolved.clone(), dirty)
            && (previous || dirty)
        {
            return Some(path);
        }
        if remote {
            continue;
        }
        #[cfg(unix)]
        if let Ok(metadata) = std::fs::metadata(&resolved) {
            use std::os::unix::fs::MetadataExt;
            if let Some(previous) = files.insert((metadata.dev(), metadata.ino()), dirty)
                && (previous || dirty)
            {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn file_aliases_cannot_bypass_the_duplicate_copy_guard() {
        let dir = db::testing::dir("quit-file-aliases");
        let original = dir.join("original.txt");
        let other = dir.join("other.txt");
        let linked = dir.join("linked.txt");
        let hard = dir.join("hard.txt");
        std::fs::write(&original, "one").unwrap();
        std::fs::write(&other, "two").unwrap();
        std::os::unix::fs::symlink(&original, &linked).unwrap();
        std::fs::hard_link(&original, &hard).unwrap();
        assert!(duplicate_file(vec![(original.clone(), true), (other, false)]).is_none());
        assert!(duplicate_file(vec![(original.clone(), true), (linked, false)]).is_some());
        assert!(duplicate_file(vec![(original, false), (hard, true)]).is_some());
    }
}

impl Workspace {
    pub(super) fn pending_saves(&self, cx: &App) -> Vec<Shared<Task<bool>>> {
        self.documents(cx)
            .into_iter()
            .filter_map(|doc| doc.read(cx).pending_save())
            .chain(
                self.notebooks
                    .iter()
                    .filter_map(|(book, _)| book.read(cx).pending_save()),
            )
            .collect()
    }
    pub(super) fn save_dirty(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Task<bool>> {
        let editors: Vec<_> = self
            .documents(cx)
            .into_iter()
            .filter(|doc| doc.read(cx).is_dirty())
            .filter_map(|doc| {
                self.all_editors()
                    .find(|editor| editor.read(cx).document() == &doc)
                    .cloned()
            })
            .collect();
        let mut tasks: Vec<_> = editors
            .iter()
            .map(|editor| self.save_editor(editor, window, cx))
            .collect();
        for (book, _) in &self.notebooks {
            if book.read(cx).is_dirty() {
                tasks.push(book.update(cx, |book, cx| book.save(cx)));
            }
        }
        tasks
    }
}
