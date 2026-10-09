//! File tree. Directories are read only when expanded, so opening a monorepo
//! costs one `read_dir` of the root, not a walk of `node_modules`.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    MouseButton, MouseDownEvent, Pixels, Point, PromptLevel, ScrollStrategy, SharedString,
    Subscription, UniformListScrollHandle, Window, actions, anchored, deferred, div, prelude::*,
    px, uniform_list,
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
}

const ROW: Pixels = px(24.);

fn read_dir(dir: &Path, depth: usize) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if matches!(name.as_str(), ".git" | ".DS_Store") {
                return None;
            }
            // Follow symlinks so a linked folder expands like a folder.
            let is_dir = std::fs::metadata(e.path())
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
    entries
}

impl ProjectPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let entries = read_dir(&root, 0);
        Self {
            root,
            focus_handle: cx.focus_handle(),
            entries,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            menu: None,
            edit: None,
            _edit_subscription: None,
            tints: Default::default(),
        }
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
        let expanded: HashSet<PathBuf> = self
            .entries
            .iter()
            .filter(|e| e.expanded)
            .map(|e| e.path.clone())
            .collect();
        let mut out = Vec::new();
        fn walk(dir: &Path, depth: usize, expanded: &HashSet<PathBuf>, out: &mut Vec<Entry>) {
            for mut entry in read_dir(dir, depth) {
                let open = entry.is_dir && expanded.contains(&entry.path);
                entry.expanded = open;
                let path = entry.path.clone();
                out.push(entry);
                if open {
                    walk(&path, depth + 1, expanded, out);
                }
            }
        }
        walk(&self.root, 0, &expanded, &mut out);
        self.entries = out;
        cx.notify();
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
            if let Some(ix) = self.index_of(&dir)
                && !self.entries[ix].expanded
            {
                self.expand(ix);
            }
        }
        self.select_path(path, cx);
    }

    pub fn select_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.selected = Some(path.to_path_buf());
        if let Some(ix) = self.index_of(path) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected.as_deref().and_then(|p| self.index_of(p))
    }

    fn expand(&mut self, ix: usize) {
        let entry = &self.entries[ix];
        if !entry.is_dir || entry.expanded {
            return;
        }
        let children = read_dir(&entry.path, entry.depth + 1);
        self.entries.splice(ix + 1..ix + 1, children);
        self.entries[ix].expanded = true;
    }

    fn collapse(&mut self, ix: usize) {
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
    }

    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(ix) else {
            return;
        };
        self.selected = Some(entry.path.clone());
        if entry.is_dir {
            if entry.expanded {
                self.collapse(ix);
            } else {
                self.expand(ix);
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
            self.expand(ix);
            cx.notify();
        }
    }

    fn collapse_selected(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_index() else {
            return;
        };
        if self.entries[ix].expanded {
            self.collapse(ix);
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
            Some(p) if p.is_dir() => p.to_path_buf(),
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
            self.expand(ix);
        }
        let editor = cx.new(|cx| {
            let mut e = Editor::single_line(
                match kind {
                    EditKind::NewFolder => "Folder name",
                    _ => "File name",
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
        self.edit = Some(EditState {
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
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let name = edit.editor.read(cx).text(cx).trim().to_string();
        if name.is_empty() || name.contains(['/', '\\']) && edit.kind == EditKind::Rename {
            edit.error = Some("Enter a valid name".into());
            cx.notify();
            return;
        }
        let result = match edit.kind {
            EditKind::NewFile => {
                let path = edit.target.join(&name);
                if path.exists() {
                    Err(format!("{name} already exists"))
                } else {
                    path.parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|_| std::fs::write(&path, ""))
                        .map(|_| Some(path))
                        .map_err(|e| e.to_string())
                }
            }
            EditKind::NewFolder => {
                let path = edit.target.join(&name);
                std::fs::create_dir_all(&path)
                    .map(|_| Some(path))
                    .map_err(|e| e.to_string())
            }
            EditKind::Rename => {
                let from = edit.target.clone();
                let to = from.with_file_name(&name);
                if to == from {
                    Ok(None)
                } else if to.exists() {
                    Err(format!("{name} already exists"))
                } else {
                    std::fs::rename(&from, &to)
                        .map(|_| {
                            cx.emit(ProjectPanelEvent::Renamed {
                                from,
                                to: to.clone(),
                            });
                            Some(to)
                        })
                        .map_err(|e| e.to_string())
                }
            }
        };
        match result {
            Ok(path) => {
                let kind = edit.kind;
                self.edit = None;
                self._edit_subscription = None;
                self.refresh(cx);
                if let Some(path) = path {
                    self.reveal(&path, cx);
                    if kind == EditKind::NewFile {
                        cx.emit(ProjectPanelEvent::OpenFile(path));
                    } else {
                        window.focus(&self.focus_handle);
                    }
                } else {
                    window.focus(&self.focus_handle);
                }
            }
            Err(err) => {
                if let Some(edit) = self.edit.as_mut() {
                    edit.error = Some(err);
                }
                cx.notify();
            }
        }
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
        let Some(path) = self.selected.clone() else {
            return;
        };
        self.menu = None;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Move \u{201c}{name}\u{201d} to the Trash?"),
            Some("You can restore it from the Trash."),
            &["Move to Trash", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let result = cx
                .background_executor()
                .spawn({
                    let path = path.clone();
                    async move { trash::delete(&path) }
                })
                .await;
            this.update(cx, |this, cx| {
                if let Err(err) = result {
                    eprintln!("could not move {} to trash: {err}", path.display());
                }
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
    }

    fn copy_path(&mut self, _: &CopyPath, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = &self.selected {
            cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
        }
        self.menu = None;
        cx.notify();
    }

    fn reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
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
                .rounded(px(6.))
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
        Some(deferred(
            anchored().position(position).child(
                div()
                    .occlude()
                    .w(px(200.))
                    .p_1()
                    .flex()
                    .flex_col()
                    .bg(theme.bg_elev)
                    .border_1()
                    .border_color(theme.line)
                    .rounded(px(8.))
                    .shadow_lg()
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        cx.notify();
                    }))
                    .child(item("m-new-file", "New file", Box::new(NewFile), true))
                    .child(item(
                        "m-new-folder",
                        "New folder",
                        Box::new(NewFolder),
                        true,
                    ))
                    .child(separator())
                    .child(item("m-rename", "Rename", Box::new(Rename), has_target))
                    .child(item(
                        "m-delete",
                        "Move to Trash",
                        Box::new(Delete),
                        has_target,
                    ))
                    .child(separator())
                    .child(item("m-copy", "Copy path", Box::new(CopyPath), has_target))
                    .child(item(
                        "m-reveal",
                        "Reveal in Finder",
                        Box::new(RevealInFinder),
                        true,
                    )),
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
                                                .rounded(px(6.))
                                                .border_1()
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
                                    .rounded(px(8.))
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
            .children(self.edit.as_ref().and_then(|e| e.error.clone()).map(|err| {
                div()
                    .mx_2()
                    .mb_2()
                    .px_2()
                    .py_1()
                    .rounded(px(8.))
                    .bg(theme.bg_elev)
                    .border_1()
                    .border_color(theme.error)
                    .text_size(crate::theme::text(11.5))
                    .text_color(theme.error)
                    .child(err)
            }))
            .children(self.render_menu(cx))
    }
}
