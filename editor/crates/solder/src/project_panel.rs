//! File tree. Directories are read only when expanded, so opening a monorepo
//! costs one `read_dir` of the root, not a walk of `node_modules`.

use crate::lang::{t, tf};
use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    MouseButton, MouseDownEvent, Pixels, Point, PromptLevel, ScrollStrategy, SharedString,
    Subscription, Task, UniformListScrollHandle, Window, actions, anchored, deferred, div,
    prelude::*, px, uniform_list,
};

use crate::{
    editor::Editor,
    theme::{ActiveTheme, UI_FONT_SIZE},
};

actions!(
    project_panel,
    [
        SelectPrev,
        SelectNext,
        Expand,
        Collapse,
        Open,
        NewFile,
        NewFolder,
        Rename,
        Delete,
        CopyPath,
        RevealInFinder,
        ConfirmEdit,
        CancelEdit,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("ProjectPanel && !editing");
    let edit = Some("ProjectPanel && editing");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, ctx),
        KeyBinding::new("down", SelectNext, ctx),
        KeyBinding::new("right", Expand, ctx),
        KeyBinding::new("left", Collapse, ctx),
        KeyBinding::new("enter", Open, ctx),
        KeyBinding::new("space", Open, ctx),
        KeyBinding::new("f2", Rename, ctx),
        KeyBinding::new("secondary-backspace", Delete, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("secondary-alt-c", CopyPath, ctx),
        KeyBinding::new("enter", ConfirmEdit, edit),
        KeyBinding::new("escape", CancelEdit, edit),
    ]);
}

pub enum ProjectPanelEvent {
    OpenFile(PathBuf),
    /// A file or folder was renamed; open editors should follow it.
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
}

impl EventEmitter<ProjectPanelEvent> for ProjectPanel {}

struct Entry {
    path: PathBuf,
    name: SharedString,
    depth: usize,
    is_dir: bool,
    expanded: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    Rename,
    NewFile,
    NewFolder,
}

struct EditState {
    revision: u64,
    kind: EditKind,
    /// The renamed entry, or the directory a new entry goes into.
    target: PathBuf,
    editor: Entity<Editor>,
    error: Option<String>,
}

pub struct ProjectPanel {
    root: PathBuf,
    focus_handle: FocusHandle,
    /// Flattened visible tree, in display order.
    entries: Vec<Entry>,
    selected: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    menu: Option<(Point<Pixels>, Option<PathBuf>)>,
    edit: Option<EditState>,
    _edit_subscription: Option<Subscription>,
    tints: std::collections::HashMap<PathBuf, crate::git_store::Tint>,
    _decorations: Option<Subscription>,
    expanded: HashSet<PathBuf>,
    tree_revision: u64,
    tree_task: Option<Task<()>>,
    scroll_to_selected: bool,
    edit_revision: u64,
    mutating: bool,
    error: Option<String>,
    tree_error: Option<String>,
}

const ROW: Pixels = px(24.);

fn read_dir(dir: &Path, depth: usize, mirror: bool) -> io::Result<Vec<Entry>> {
    let read = std::fs::read_dir(dir)?;
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if matches!(name.as_str(), ".git" | ".DS_Store") {
                return None;
            }
            // Follow symlinks so a linked folder expands like a folder.
            let is_dir = if mirror {
                e.file_type()
            } else {
                std::fs::metadata(e.path()).map(|m| m.file_type())
            }
            .map(|m| m.is_dir())
            .unwrap_or(false);
            Some(Entry {
                path: e.path(),
                name: name.into(),
                depth,
                is_dir,
                expanded: false,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

fn read_tree(root: &Path, expanded: &HashSet<PathBuf>) -> io::Result<Vec<Entry>> {
    // SSH cache entries describe names and kinds only. Never inspect their bytes
    // or try a local tree when the registered connection is gone.
    let mount = crate::remote::mount(root)?;
    let mirror = mount.as_ref().is_some_and(|mount| mount.is_mirror());
    fn walk(
        dir: &Path,
        depth: usize,
        expanded: &HashSet<PathBuf>,
        mirror: bool,
        out: &mut Vec<Entry>,
    ) -> io::Result<()> {
        for mut entry in read_dir(dir, depth, mirror)? {
            let open = entry.is_dir && expanded.contains(&entry.path);
            entry.expanded = open;
            let path = entry.path.clone();
            out.push(entry);
            if open {
                match walk(&path, depth + 1, expanded, mirror, out) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    result => result?,
                }
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, 0, expanded, mirror, &mut out)?;
    Ok(out)
}

fn valid_name(name: &str, kind: EditKind) -> bool {
    if name.is_empty()
        || name.contains('\0')
        || kind == EditKind::Rename && name.contains(['/', '\\'])
    {
        return false;
    }
    name.split(['/', '\\'])
        .all(|part| !part.is_empty() && !matches!(part, "." | ".."))
}

fn same_environment(root: &Path, expected: &Option<Arc<crate::remote::Mount>>) -> io::Result<()> {
    match (crate::remote::mount(root)?, expected) {
        (None, None) => Ok(()),
        (Some(found), Some(expected)) if Arc::ptr_eq(&found, expected) => Ok(()),
        _ => Err(io::ErrorKind::NotConnected.into()),
    }
}

/// Blocking file operations. Existence checks must use the target, since an
/// unopened SSH file in the inventory can still be an empty placeholder.
fn apply_edit(
    root: &Path,
    kind: EditKind,
    target: &Path,
    name: &str,
) -> io::Result<Option<PathBuf>> {
    if !valid_name(name, kind)
        || target.strip_prefix(root).map_or(true, |relative| {
            relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        })
        || kind == EditKind::Rename && target == root
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid project entry",
        ));
    }
    let mount = crate::remote::mount(target)?;
    let path = if kind == EditKind::Rename {
        target.with_file_name(name)
    } else {
        target.join(name.replace('\\', "/"))
    };
    if kind == EditKind::Rename && path == target {
        return Ok(None);
    }
    match crate::remote::metadata(&path) {
        Ok(_) if kind != EditKind::NewFolder => return Err(io::ErrorKind::AlreadyExists.into()),
        Ok(metadata) if !metadata.is_dir() => return Err(io::ErrorKind::AlreadyExists.into()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match kind {
        EditKind::NewFile => {
            if let Some(parent) = path.parent() {
                crate::remote::create_dir_all(parent)?;
            }
            if mount.as_ref().is_some_and(|mount| mount.is_mirror()) {
                crate::remote::write_versioned(&path, b"", None)?;
            } else {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
            }
        }
        EditKind::NewFolder => crate::remote::create_dir_all(&path)?,
        EditKind::Rename => crate::remote::rename(target, &path)?,
    }
    Ok(Some(path))
}

fn delete_entry(path: &Path, permanent: bool) -> io::Result<()> {
    let mount = crate::remote::mount(path)?;
    if mount.as_ref().is_some_and(|mount| mount.is_mirror()) {
        if !permanent {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if crate::remote::metadata(path)?.is_dir() {
            crate::remote::remove_dir_all(path)
        } else {
            crate::remote::remove_file(path)
        }
    } else if permanent {
        // The environment changed after the confirmation was displayed.
        Err(io::ErrorKind::NotConnected.into())
    } else {
        trash::delete(path).map_err(io::Error::other)
    }
}

impl ProjectPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let decorations = crate::extension_store::ExtensionStore::try_global(cx).map(|store| {
            cx.subscribe(&store, |_, _, event, cx| {
                if matches!(event, crate::extension_api::ExtensionEvent::Files) {
                    cx.notify();
                }
            })
        });
        let mut panel = Self {
            root,
            focus_handle: cx.focus_handle(),
            entries: Vec::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            menu: None,
            edit: None,
            _edit_subscription: None,
            tints: Default::default(),
            _decorations: decorations,
            expanded: HashSet::new(),
            tree_revision: 0,
            tree_task: None,
            scroll_to_selected: false,
            edit_revision: 0,
            mutating: false,
            error: None,
            tree_error: None,
        };
        panel.refresh(cx);
        panel
    }

    /// Git colors for changed files and the folders that contain them.
    pub fn set_tints(
        &mut self,
        tints: std::collections::HashMap<PathBuf, crate::git_store::Tint>,
        cx: &mut Context<Self>,
    ) {
        if tints != self.tints {
            self.tints = tints;
            cx.notify();
        }
    }

    /// Re-reads every expanded directory, keeping expansion and selection.
    /// Called when files change on disk.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.tree_revision = self.tree_revision.wrapping_add(1);
        let revision = self.tree_revision;
        let (root, expanded) = (self.root.clone(), self.expanded.clone());
        self.tree_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { read_tree(&root, &expanded) })
                .await;
            this.update(cx, |this, cx| {
                if this.tree_revision != revision {
                    return;
                }
                match result {
                    Ok(entries) => {
                        this.tree_error = None;
                        this.entries = entries;
                        if this.scroll_to_selected
                            && let Some(ix) = this.selected_index()
                        {
                            this.scroll.scroll_to_item(ix, ScrollStrategy::Center);
                            this.scroll_to_selected = false;
                        }
                    }
                    Err(error) => {
                        this.tree_error = Some(tf("Could not refresh files: {}", &[&error]))
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Expands the ancestors of `path` and selects it.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return;
        };
        let mut dir = self.root.clone();
        let components: Vec<_> = rel.components().collect();
        for c in components.iter().take(components.len().saturating_sub(1)) {
            dir.push(c);
            self.expanded.insert(dir.clone());
        }
        self.select_path(path, cx);
        self.refresh(cx);
    }

    pub fn select_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.selected = Some(path.to_path_buf());
        if let Some(ix) = self.index_of(path) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
            self.scroll_to_selected = false;
        } else {
            self.scroll_to_selected = true;
        }
        cx.notify();
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected.as_deref().and_then(|p| self.index_of(p))
    }

    fn expand(&mut self, ix: usize, cx: &mut Context<Self>) {
        let entry = &self.entries[ix];
        if !entry.is_dir || entry.expanded {
            return;
        }
        self.expanded.insert(entry.path.clone());
        self.entries[ix].expanded = true;
        self.refresh(cx);
    }

    fn collapse(&mut self, ix: usize, cx: &mut Context<Self>) {
        let depth = self.entries[ix].depth;
        if !self.entries[ix].expanded {
            return;
        }
        let end = self.entries[ix + 1..]
            .iter()
            .position(|e| e.depth <= depth)
            .map_or(self.entries.len(), |p| ix + 1 + p);
        self.entries.drain(ix + 1..end);
        self.entries[ix].expanded = false;
        let path = self.entries[ix].path.clone();
        self.expanded.retain(|entry| !entry.starts_with(&path));
        // Invalidate a directory read that began before the collapse.
        self.refresh(cx);
    }

    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(ix) else {
            return;
        };
        self.selected = Some(entry.path.clone());
        if entry.is_dir {
            if entry.expanded {
                self.collapse(ix, cx);
            } else {
                self.expand(ix, cx);
            }
        } else {
            cx.emit(ProjectPanelEvent::OpenFile(entry.path.clone()));
        }
        cx.notify();
    }

    // ------------------------------------------------------------ keyboard

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected_index().map_or(0, |i| i.saturating_sub(1));
        if let Some(e) = self.entries.get(ix) {
            let path = e.path.clone();
            self.select_path(&path, cx);
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self
            .selected_index()
            .map_or(0, |i| (i + 1).min(self.entries.len().saturating_sub(1)));
        if let Some(e) = self.entries.get(ix) {
            let path = e.path.clone();
            self.select_path(&path, cx);
        }
    }

    fn expand_selected(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected_index() {
            self.expand(ix, cx);
            cx.notify();
        }
    }

    fn collapse_selected(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_index() else {
            return;
        };
        if self.entries[ix].expanded {
            self.collapse(ix, cx);
        } else if let Some(parent) = self.entries[ix].path.parent().map(Path::to_path_buf) {
            // Already collapsed: jump to the parent folder.
            if parent != self.root {
                self.select_path(&parent, cx);
            }
        }
        cx.notify();
    }

    fn open_selected(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected_index() {
            self.activate(ix, cx);
        }
    }

    // ------------------------------------------------------------ file ops

    /// The folder new entries go into: the selection, or its parent for a file.
    fn target_dir(&self, path: Option<&Path>) -> PathBuf {
        match path {
            Some(p)
                if p == self.root || self.index_of(p).is_some_and(|ix| self.entries[ix].is_dir) =>
            {
                p.to_path_buf()
            }
            Some(p) => p.parent().map_or(self.root.clone(), Path::to_path_buf),
            None => self.root.clone(),
        }
    }

    fn start_edit(
        &mut self,
        kind: EditKind,
        target: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let initial = match kind {
            EditKind::Rename => target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        if kind != EditKind::Rename
            && let Some(ix) = self.index_of(&target)
        {
            self.expand(ix, cx);
        }
        let editor = cx.new(|cx| {
            let mut e = Editor::single_line(
                match kind {
                    EditKind::NewFolder => t("Folder name"),
                    _ => t("File name"),
                },
                cx,
            );
            e.set_text(&initial, false, cx);
            // Select the stem so typing replaces the name but keeps the extension.
            let stem = initial
                .rfind('.')
                .filter(|i| *i > 0)
                .unwrap_or(initial.len());
            e.select_range(0..stem, cx);
            e
        });
        window.focus(&editor.focus_handle(cx));
        // Losing focus cancels, like a native rename field.
        self._edit_subscription =
            Some(
                cx.on_blur(&editor.focus_handle(cx), window, |this, window, cx| {
                    if this.edit.is_some() {
                        this.cancel_edit(&CancelEdit, window, cx);
                    }
                }),
            );
        self.edit_revision = self.edit_revision.wrapping_add(1);
        self.error = None;
        self.edit = Some(EditState {
            revision: self.edit_revision,
            kind,
            target,
            editor,
            error: None,
        });
        cx.notify();
    }

    fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.target_dir(self.selected.clone().as_deref());
        self.start_edit(EditKind::NewFile, dir, window, cx);
    }

    fn new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.target_dir(self.selected.clone().as_deref());
        self.start_edit(EditKind::NewFolder, dir, window, cx);
    }

    fn rename(&mut self, _: &Rename, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.selected.clone() {
            self.start_edit(EditKind::Rename, path, window, cx);
        }
    }

    fn confirm_edit(&mut self, _: &ConfirmEdit, window: &mut Window, cx: &mut Context<Self>) {
        if self.mutating {
            return;
        }
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let name = edit.editor.read(cx).text(cx).trim().to_string();
        if !valid_name(&name, edit.kind) {
            edit.error = Some(t("Enter a valid name").into());
            cx.notify();
            return;
        }
        let (revision, kind, target) = (edit.revision, edit.kind, edit.target.clone());
        edit.error = None;
        self.mutating = true;
        let root = self.root.clone();
        let environment = crate::remote::mount(&root);
        cx.spawn_in(window, async move |this, cx| {
            let environment = match environment {
                Ok(environment) => environment,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.mutating = false;
                        this.error = Some(tf("Could not change files: {}", &[&error]));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let result = cx
                .background_executor()
                .spawn({
                    let (root, target, name, environment) = (
                        root.clone(),
                        target.clone(),
                        name.clone(),
                        environment.clone(),
                    );
                    async move {
                        same_environment(&root, &environment)?;
                        apply_edit(&root, kind, &target, &name)
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.mutating = false;
                if same_environment(&this.root, &environment).is_err() {
                    this.refresh(cx);
                    return;
                }
                // The operation already happened. Open tabs must follow a rename,
                // even if focus loss cancelled its inline field while IO ran.
                if let Ok(Some(to)) = &result
                    && kind == EditKind::Rename
                {
                    cx.emit(ProjectPanelEvent::Renamed {
                        from: target,
                        to: to.clone(),
                    });
                }
                this.refresh(cx);
                if this
                    .edit
                    .as_ref()
                    .is_none_or(|edit| edit.revision != revision)
                {
                    if let Err(error) = result {
                        this.error = Some(tf("Could not change files: {}", &[&error]));
                    }
                    cx.notify();
                    return;
                }
                match result {
                    Ok(path) => {
                        this.edit = None;
                        this._edit_subscription = None;
                        if let Some(path) = path {
                            this.reveal(&path, cx);
                            if kind == EditKind::NewFile {
                                cx.emit(ProjectPanelEvent::OpenFile(path));
                            } else {
                                window.focus(&this.focus_handle);
                            }
                        } else {
                            window.focus(&this.focus_handle);
                        }
                    }
                    Err(error) => {
                        if let Some(edit) = this.edit.as_mut() {
                            edit.error = Some(if error.kind() == io::ErrorKind::AlreadyExists {
                                tf("{} already exists", &[&name])
                            } else {
                                tf("Could not change files: {}", &[&error])
                            });
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn cancel_edit(&mut self, _: &CancelEdit, window: &mut Window, cx: &mut Context<Self>) {
        let was_focused = self
            .edit
            .as_ref()
            .is_some_and(|e| e.editor.focus_handle(cx).is_focused(window));
        self.edit = None;
        self._edit_subscription = None;
        if was_focused {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.mutating {
            return;
        }
        let Some(path) = self.selected.clone() else {
            return;
        };
        self.menu = None;
        self.error = None;
        self.mutating = true;
        // A closed SSH namespace still requires the permanent-delete wording;
        // the background wrapper will report the connection error.
        let permanent = self.is_mirror();
        let environment = crate::remote::mount(&self.root);
        let root = self.root.clone();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let message = if permanent {
            tf("Permanently delete \u{201c}{}\u{201d}?", &[&name])
        } else {
            tf("Move \u{201c}{}\u{201d} to the Trash?", &[&name])
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(if permanent {
                t("This cannot be undone.")
            } else {
                t("You can restore it from the Trash.")
            }),
            &[
                if permanent {
                    t("Delete permanently")
                } else {
                    t("Move to Trash")
                },
                t("Cancel"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                this.update(cx, |this, cx| {
                    this.mutating = false;
                    cx.notify();
                })
                .ok();
                return;
            }
            let result = cx
                .background_executor()
                .spawn({
                    let path = path.clone();
                    async move {
                        let environment = environment?;
                        same_environment(&root, &environment)?;
                        delete_entry(&path, permanent)
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.mutating = false;
                if let Err(err) = result {
                    crate::journal::error(format_args!(
                        "could not delete {}: {err}",
                        path.display()
                    ));
                    this.error = Some(tf("Could not delete {}: {}", &[&name, &err]));
                }
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
    }

    fn copy_path(&mut self, _: &CopyPath, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = &self.selected {
            match crate::remote::mount(path) {
                Ok(Some(mount)) => match mount.remote_path(path) {
                    Ok(path) => cx.write_to_clipboard(ClipboardItem::new_string(path)),
                    Err(error) => self.error = Some(tf("Could not copy path: {}", &[&error])),
                },
                Ok(None) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()))
                }
                Err(error) => self.error = Some(tf("Could not copy path: {}", &[&error])),
            }
        }
        self.menu = None;
        cx.notify();
    }

    fn reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_mirror() {
            return;
        }
        let path = self.selected.clone().unwrap_or_else(|| self.root.clone());
        cx.reveal_path(&path);
        self.menu = None;
        cx.notify();
    }

    fn open_menu(
        &mut self,
        position: Point<Pixels>,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(p) = &path {
            self.selected = Some(p.clone());
        }
        self.menu = Some((position, path));
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (position, path) = self.menu.clone()?;
        let theme = cx.theme().clone();
        let has_target = path.is_some();
        let mirror = self.is_mirror();
        let item = |id: &'static str,
                    label: &'static str,
                    action: Box<dyn gpui::Action>,
                    enabled: bool| {
            let theme = theme.clone();
            div()
                .id(id)
                .h(px(26.))
                .px_2()
                .flex()
                .items_center()
                .rounded(theme.shape.token)
                .text_size(UI_FONT_SIZE)
                .text_color(if enabled { theme.fg } else { theme.fg_subtle })
                .when(enabled, |d| d.hover(|d| d.bg(theme.accent_soft)))
                .child(label)
                .when(enabled, move |d| {
                    d.on_click(move |_, window, cx| {
                        window.dispatch_action(action.boxed_clone(), cx)
                    })
                })
        };
        let separator = || div().my_1().h(px(1.)).bg(theme.line);
        // What extensions put in this menu for the file it is on.
        let offered = path.as_deref().map_or_else(Vec::new, |path| {
            let Some(store) = crate::extension_store::ExtensionStore::try_global(cx) else {
                return Vec::new();
            };
            let store = store.read(cx);
            let mut facts = store.facts();
            let part = |part: Option<&std::ffi::OsStr>| {
                part.map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            let ending = match part(path.extension()) {
                ending if ending.is_empty() => ending,
                ending => format!(".{ending}"),
            };
            for (name, value) in [
                ("resourceFilename", part(path.file_name()).into()),
                ("resourceExtname", ending.into()),
                ("resourceScheme", "file".into()),
                (
                    "explorerResourceIsFolder",
                    self.index_of(path)
                        .is_some_and(|ix| self.entries[ix].is_dir)
                        .into(),
                ),
            ] {
                facts.insert(name.into(), value);
            }
            store.menu("explorer/context", &facts, Some(path))
        });
        let theirs = offered.into_iter().enumerate().map(|(i, offered)| {
            let (theme, action) = (theme.clone(), offered.action);
            div()
                .id(("m-extension", i))
                .debug_selector(move || format!("m-extension-{i}"))
                .h(px(26.))
                .px_2()
                .flex()
                .items_center()
                .rounded(theme.shape.token)
                .text_size(UI_FONT_SIZE)
                .text_color(theme.fg)
                .hover(|d| d.bg(theme.accent_soft))
                .child(div().truncate().child(offered.title))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.menu = None;
                    window.dispatch_action(Box::new(action.clone()), cx);
                    cx.notify();
                }))
        });
        let theirs: Vec<_> = theirs.collect();
        Some(deferred(
            anchored().position(position).child(
                div()
                    .occlude()
                    .w(px(200.))
                    .p_1()
                    .flex()
                    .flex_col()
                    .bg(theme.bg_elev)
                    .border(theme.shape.border)
                    .border_color(theme.line)
                    .rounded(theme.shape.control)
                    .shadow_lg()
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        cx.notify();
                    }))
                    .child(item("m-new-file", t("New file"), Box::new(NewFile), true))
                    .child(item(
                        "m-new-folder",
                        t("New folder"),
                        Box::new(NewFolder),
                        true,
                    ))
                    .child(separator())
                    .child(item("m-rename", t("Rename"), Box::new(Rename), has_target))
                    .child(item(
                        "m-delete",
                        if mirror {
                            t("Delete permanently")
                        } else {
                            t("Move to Trash")
                        },
                        Box::new(Delete),
                        has_target,
                    ))
                    .child(separator())
                    .child(item(
                        "m-copy",
                        t("Copy path"),
                        Box::new(CopyPath),
                        has_target,
                    ))
                    .child(item(
                        "m-reveal",
                        t("Reveal in Finder"),
                        Box::new(RevealInFinder),
                        !mirror,
                    ))
                    .when(!theirs.is_empty(), |d| d.child(separator()))
                    .children(theirs),
            ),
        ))
    }

    /// Rows as displayed: entries, plus a pending new entry under its folder.
    fn display_rows(&self) -> Vec<Option<usize>> {
        let mut rows: Vec<Option<usize>> = (0..self.entries.len()).map(Some).collect();
        if let Some(edit) = &self.edit
            && edit.kind != EditKind::Rename
        {
            let at = if edit.target == self.root {
                0
            } else {
                self.index_of(&edit.target).map_or(0, |i| i + 1)
            };
            rows.insert(at, None);
        }
        rows
    }

    fn is_mirror(&self) -> bool {
        match crate::remote::mount(&self.root) {
            Ok(Some(mount)) => mount.is_mirror(),
            Ok(None) => false,
            Err(_) => true,
        }
    }
}

impl Focusable for ProjectPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProjectPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = self.display_rows();
        let panel_focused = self.focus_handle.contains_focused(window, cx);
        div()
            .key_context(if self.edit.is_some() {
                "ProjectPanel editing"
            } else {
                "ProjectPanel"
            })
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::expand_selected))
            .on_action(cx.listener(Self::collapse_selected))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::new_folder))
            .on_action(cx.listener(Self::rename))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::copy_path))
            .on_action(cx.listener(Self::reveal_in_finder))
            .on_action(cx.listener(Self::confirm_edit))
            .on_action(cx.listener(Self::cancel_edit))
            .size_full()
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    this.open_menu(e.position, None, window, cx)
                }),
            )
            .child(
                uniform_list(
                    "project-entries",
                    rows.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let theme = cx.theme().clone();
                        let rows = this.display_rows();
                        let paths = range
                            .clone()
                            .filter_map(|row_ix| {
                                rows[row_ix].map(|ix| this.entries[ix].path.clone())
                            })
                            .collect();
                        crate::extension_decorations::ask_visible(paths, cx);
                        range
                            .map(|row_ix| {
                                let row = rows[row_ix];
                                let (depth, name, is_dir, expanded, path) = match row {
                                    Some(ix) => {
                                        let e = &this.entries[ix];
                                        (
                                            e.depth,
                                            Some(e.name.clone()),
                                            e.is_dir,
                                            e.expanded,
                                            Some(e.path.clone()),
                                        )
                                    }
                                    None => {
                                        let edit = this.edit.as_ref().unwrap();
                                        let depth = if edit.target == this.root {
                                            0
                                        } else {
                                            this.index_of(&edit.target)
                                                .map_or(0, |i| this.entries[i].depth + 1)
                                        };
                                        (depth, None, edit.kind == EditKind::NewFolder, false, None)
                                    }
                                };
                                let selected = path.is_some() && this.selected == path;
                                let tint =
                                    path.as_ref().and_then(|p| this.tints.get(p)).map(
                                        |t| match t {
                                            crate::git_store::Tint::Added => theme.git_added,
                                            crate::git_store::Tint::Modified => theme.git_modified,
                                            crate::git_store::Tint::Conflict => theme.error,
                                        },
                                    );
                                let renaming = this.edit.as_ref().filter(|e| {
                                    e.kind == EditKind::Rename && Some(&e.target) == path.as_ref()
                                });
                                let editing = if row.is_none() {
                                    this.edit.as_ref()
                                } else {
                                    renaming
                                };
                                let caret = if is_dir {
                                    if expanded { "▾" } else { "▸" }
                                } else {
                                    ""
                                };
                                // The picture of the icon theme in use.
                                let icon = path.as_deref().and_then(|path| {
                                    if is_dir {
                                        crate::file_icons::folder(path, expanded, cx)
                                    } else {
                                        crate::file_icons::file(path, cx)
                                    }
                                });
                                let label = match editing {
                                    Some(edit) => div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .child(
                                            div()
                                                .h(px(20.))
                                                .px_1()
                                                .rounded(theme.shape.token)
                                                .border(theme.shape.border)
                                                .border_color(if edit.error.is_some() {
                                                    theme.error
                                                } else {
                                                    theme.accent
                                                })
                                                .bg(theme.bg)
                                                .flex()
                                                .items_center()
                                                .child(edit.editor.clone()),
                                        )
                                        .into_any_element(),
                                    None => div()
                                        .truncate()
                                        .child(name.unwrap_or_default())
                                        .into_any_element(),
                                };
                                div()
                                    .id(row_ix)
                                    .debug_selector(move || format!("file-row-{row_ix}"))
                                    .h(crate::theme::row(ROW, cx))
                                    .mx_1p5()
                                    .pl(px(8. + depth as f32 * 12.))
                                    .pr_2()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .rounded(theme.shape.control)
                                    .text_size(UI_FONT_SIZE)
                                    .text_color(tint.unwrap_or(if selected {
                                        theme.fg
                                    } else {
                                        theme.fg_muted
                                    }))
                                    .when(selected, |d| {
                                        d.bg(if panel_focused {
                                            theme.accent_soft
                                        } else {
                                            theme.line
                                        })
                                    })
                                    .hover(|d| d.text_color(theme.fg))
                                    .child(
                                        div()
                                            .w(px(10.))
                                            .flex_none()
                                            .text_color(theme.fg_subtle)
                                            .child(caret),
                                    )
                                    .when_some(icon, |d, icon| {
                                        d.child(
                                            div()
                                                .flex_none()
                                                .debug_selector(move || {
                                                    format!("file-icon-{row_ix}")
                                                })
                                                .child(icon),
                                        )
                                    })
                                    .children(
                                        path.as_deref()
                                            .into_iter()
                                            .flat_map(|path| {
                                                crate::extension_store::ExtensionStore::try_global(
                                                    cx,
                                                )
                                                .map(|store| store.read(cx).file_marks(path))
                                                .unwrap_or_default()
                                            })
                                            .enumerate()
                                            .map(|(i, mark)| {
                                                div()
                                                    .id(("extension-file-mark", row_ix * 100 + i))
                                                    .text_size(crate::theme::UI_FONT_SMALL)
                                                    .text_color(theme.fg_subtle)
                                                    .child(mark.badge)
                                                    .when_some(mark.tooltip, |d, tooltip| {
                                                        d.tooltip(move |_, cx| {
                                                            cx.new(|_| {
                                                                crate::ui::Tooltip(tooltip.clone())
                                                            })
                                                            .into()
                                                        })
                                                    })
                                            }),
                                    )
                                    .child(label)
                                    .when_some(row, |d, ix| {
                                        d.on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, window, cx| {
                                                window.focus(&this.focus_handle);
                                                this.activate(ix, cx);
                                            }),
                                        )
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            cx.listener(
                                                move |this, e: &MouseDownEvent, window, cx| {
                                                    cx.stop_propagation();
                                                    let path = this.entries[ix].path.clone();
                                                    this.open_menu(
                                                        e.position,
                                                        Some(path),
                                                        window,
                                                        cx,
                                                    );
                                                },
                                            ),
                                        )
                                    })
                            })
                            .collect()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1(),
            )
            .children(
                self.edit
                    .as_ref()
                    .and_then(|e| e.error.clone())
                    .or_else(|| self.error.clone())
                    .or_else(|| self.tree_error.clone())
                    .map(|err| {
                        div()
                            .mx_2()
                            .mb_2()
                            .px_2()
                            .py_1()
                            .rounded(theme.shape.control)
                            .bg(theme.bg_elev)
                            .border(theme.shape.border)
                            .border_color(theme.error)
                            .text_size(crate::theme::text(11.5))
                            .text_color(theme.error)
                            .child(err)
                    }),
            )
            .children(self.render_menu(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_operations_refuse_parent_paths_and_preserve_existing_contents() {
        let root = db::testing::dir("tree-file-ops");
        std::fs::write(root.join("keep.txt"), "keep").unwrap();
        assert_eq!(
            apply_edit(&root, EditKind::NewFile, &root, "keep.txt")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(apply_edit(&root, EditKind::NewFolder, &root, "keep.txt").is_err());
        for name in [
            "../outside",
            "a/../outside",
            "/outside",
            "a/./b",
            "a\\..\\outside",
        ] {
            assert!(apply_edit(&root, EditKind::NewFile, &root, name).is_err());
        }
        let from = apply_edit(&root, EditKind::NewFile, &root, "notes/a.txt")
            .unwrap()
            .unwrap();
        assert!(from.exists());
        let to = apply_edit(&root, EditKind::Rename, &from, "b.txt")
            .unwrap()
            .unwrap();
        assert!(!from.exists() && to.exists());
        assert_eq!(
            std::fs::read_to_string(root.join("keep.txt")).unwrap(),
            "keep"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn queued_mutations_refuse_a_replaced_environment() {
        let root = db::testing::dir("tree-environment-guard")
            .canonicalize()
            .unwrap();
        let connect = |id: &str| {
            crate::remote::Mount::connect(
                crate::remote::Target {
                    endpoint: crate::remote::Endpoint::Container {
                        program: "docker-not-run".into(),
                        id: id.into(),
                        user: None,
                        env: Vec::new(),
                    },
                    path: "/workspace/project".into(),
                },
                root.clone(),
                false,
            )
            .unwrap()
        };
        let first = connect("first");
        assert!(same_environment(&root, &None).is_err());
        let expected = Some(first.clone());
        assert!(same_environment(&root, &expected).is_ok());
        let second = connect("second");
        assert!(same_environment(&root, &expected).is_err());
        assert!(same_environment(&root, &Some(second.clone())).is_ok());
        drop(expected);
        drop(first);
        drop(second);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ssh_tree_mutates_the_target_and_requires_permanent_delete_consent() {
        let root = db::testing::dir("tree-remote-ops").canonicalize().unwrap();
        let source = root.join("server");
        let cache = root.join("cache");
        std::fs::create_dir(&source).unwrap();
        let mount = crate::remote::fixture(&source, &cache);
        apply_edit(&cache, EditKind::NewFolder, &cache, "notes").unwrap();
        let from = apply_edit(&cache, EditKind::NewFile, &cache, "notes/a.txt")
            .unwrap()
            .unwrap();
        crate::remote::write(&from, "server content").unwrap();
        let to = apply_edit(&cache, EditKind::Rename, &from, "b.txt")
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read(source.join("notes/b.txt")).unwrap(),
            b"server content"
        );
        assert!(!source.join("notes/a.txt").exists());
        // A cache miss is not permission to overwrite an existing remote file.
        std::fs::write(source.join("external.txt"), "keep").unwrap();
        assert_eq!(
            apply_edit(&cache, EditKind::NewFile, &cache, "external.txt")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            delete_entry(&to, false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(source.join("notes/b.txt").exists());
        delete_entry(&cache.join("notes"), true).unwrap();
        assert!(!source.join("notes").exists() && !cache.join("notes").exists());
        mount.refresh(&cache).unwrap();
        assert_eq!(read_tree(&cache, &HashSet::new()).unwrap().len(), 1);
        drop(mount);
        assert!(read_tree(&cache, &HashSet::new()).is_err());
        assert!(apply_edit(&cache, EditKind::NewFile, &cache, "local-fallback.txt").is_err());
        assert!(delete_entry(&cache.join("external.txt"), false).is_err());
        assert!(delete_entry(&cache.join("external.txt"), true).is_err());
        assert_eq!(std::fs::read(source.join("external.txt")).unwrap(), b"keep");
        std::fs::remove_dir_all(root).unwrap();
    }
}
