use std::{
    any::TypeId,
    collections::{HashSet, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyView, App, Context, DismissEvent, Entity, EntityId, FocusHandle, Focusable, KeyBinding,
    ManagedView, MouseButton, PathPromptOptions, PromptLevel, SharedString, Subscription, Task,
    Window, WindowControlArea, actions, deferred, div, prelude::*, px,
};

use crate::{
    buffer_search::{self, BufferSearchBar},
    command_palette::CommandPalette,
    document::Document,
    editor::{self, Editor, EditorEvent},
    editor_lsp::LspLocation,
    file_finder::FileFinder,
    git_panel::{self, BranchPicker, GitPanel, GitPanelEvent},
    git_store::{GitStore, GitStoreEvent},
    go_to_line::GoToLine as GoToLineDelegate,
    locations::{CodeActionPicker, LocationPicker, RenamePrompt},
    lsp_store::{LspStore, from_range},
    perf::{self, Perf},
    picker::Picker,
    project::{Project, ProjectEvent},
    project_panel::{ProjectPanel, ProjectPanelEvent},
    project_search::{OpenMatch, ProjectSearch},
    settings::{self, Settings},
    terminal::{Terminal, TerminalCommand, TerminalEvent},
    theme::{ActiveTheme, UI_FONT, UI_FONT_SIZE},
};

actions!(
    workspace,
    [
        CloseTab,
        NextTab,
        PrevTab,
        ToggleHud,
        Quit,
        ToggleCommandPalette,
        ToggleFileFinder,
        GoToLine,
        Find,
        FindReplace,
        FindNext,
        FindPrev,
        ShowFiles,
        ShowSearch,
        ToggleSidebar,
        RevealActiveFile,
        NewFile,
        SaveAs,
        OpenFolder,
        OpenSettings,
        OpenKeymap,
        SplitRight,
        FocusNextPane,
        FocusPrevPane,
        ToggleTerminal,
        NewTerminal,
        ShowGit,
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("secondary-shift-]", NextTab, None),
        KeyBinding::new("secondary-shift-[", PrevTab, None),
        KeyBinding::new("secondary-alt-p", ToggleHud, None),
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-shift-p", ToggleCommandPalette, None),
        KeyBinding::new("f1", ToggleCommandPalette, None),
        KeyBinding::new("secondary-p", ToggleFileFinder, None),
        KeyBinding::new("ctrl-g", GoToLine, Some("Editor")),
        KeyBinding::new("secondary-f", Find, None),
        KeyBinding::new("secondary-alt-f", FindReplace, None),
        KeyBinding::new("secondary-g", FindNext, None),
        KeyBinding::new("secondary-shift-g", FindPrev, None),
        KeyBinding::new("secondary-shift-e", ShowFiles, None),
        KeyBinding::new("secondary-shift-f", ShowSearch, None),
        KeyBinding::new("secondary-b", ToggleSidebar, None),
        KeyBinding::new("secondary-n", NewFile, None),
        KeyBinding::new("secondary-alt-s", SaveAs, None),
        KeyBinding::new("secondary-o", OpenFolder, None),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-\\", SplitRight, None),
        KeyBinding::new("secondary-k secondary-right", FocusNextPane, None),
        KeyBinding::new("secondary-k secondary-left", FocusPrevPane, None),
        KeyBinding::new("ctrl-`", ToggleTerminal, None),
        KeyBinding::new("ctrl-shift-`", NewTerminal, None),
        KeyBinding::new("ctrl-shift-g", ShowGit, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}

const SIDEBAR_WIDTH: f32 = 260.;
const TITLEBAR_HEIGHT: f32 = 38.;
const TAB_BAR_HEIGHT: f32 = 34.;
const STATUS_HEIGHT: f32 = 26.;
const DOCK_HEIGHT: f32 = 280.;
const RECENT_LIMIT: usize = 20;

/// Where to put the cursor after opening a file.
#[derive(Clone)]
pub enum Jump {
    Point {
        row: usize,
        column: usize,
    },
    RowColumns {
        row: usize,
        columns: Range<usize>,
    },
    /// A range from a language server, in that server's position encoding.
    Lsp {
        range: lsp::types::Range,
        encoding: lsp::Encoding,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Files,
    Search,
    Git,
}

struct Modal {
    view: AnyView,
    type_id: TypeId,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    _subscription: Subscription,
}

struct Tab {
    editor: Entity<Editor>,
    _subscriptions: [Subscription; 2],
}

/// A column of tabs. The workspace lays panes out left to right.
#[derive(Default)]
struct Pane {
    tabs: Vec<Tab>,
    active: Option<usize>,
}

impl Pane {
    fn active_editor(&self) -> Option<&Entity<Editor>> {
        self.active
            .and_then(|i| self.tabs.get(i))
            .map(|t| &t.editor)
    }
}

pub struct Workspace {
    focus_handle: FocusHandle,
    project: Entity<Project>,
    project_panel: Entity<ProjectPanel>,
    project_search: Entity<ProjectSearch>,
    git: Entity<GitStore>,
    git_panel: Entity<GitPanel>,
    search_bar: Entity<BufferSearchBar>,
    panes: Vec<Pane>,
    active_pane: usize,
    recent: VecDeque<Arc<str>>,
    sidebar: Option<SidebarTab>,
    modal: Option<Modal>,
    terminals: Vec<(Entity<Terminal>, Subscription)>,
    active_terminal: usize,
    dock_open: bool,
    _subscriptions: Vec<Subscription>,
    _hud_tick: Task<()>,
}

impl Workspace {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = cx.new(|cx| Project::new(root.clone(), cx));
        let project_panel = cx.new(|cx| ProjectPanel::new(root.clone(), cx));
        let git = cx.new(|cx| GitStore::new(root.clone(), cx));
        let git_panel = cx.new(|cx| GitPanel::new(root.clone(), git.clone(), cx));
        let project_search = cx.new(|cx| ProjectSearch::new(root, window, cx));
        let search_bar = cx.new(|cx| BufferSearchBar::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &project_panel,
                window,
                |this, _, event, window, cx| match event {
                    ProjectPanelEvent::OpenFile(path) => {
                        this.open_path(path.clone(), None, window, cx)
                    }
                    ProjectPanelEvent::Renamed { from, to } => this.follow_rename(from, to, cx),
                },
            ),
            cx.subscribe_in(
                &project_search,
                window,
                |this, _, m: &OpenMatch, window, cx| {
                    let jump = Jump::RowColumns {
                        row: m.row,
                        columns: m.columns.clone(),
                    };
                    this.open_path(m.path.clone(), Some(jump), window, cx);
                },
            ),
            cx.subscribe(&project, |this, _, event, cx| match event {
                ProjectEvent::Changed(paths) => {
                    this.files_changed(paths.clone(), cx);
                    this.git.update(cx, |g, cx| g.refresh(cx));
                }
                ProjectEvent::GitChanged => this.git.update(cx, |g, cx| g.refresh(cx)),
                ProjectEvent::Scanned => cx.notify(),
            }),
            cx.subscribe(&git, |this, _, event, cx| match event {
                GitStoreEvent::StatusChanged => this.git_status_changed(cx),
            }),
            cx.subscribe_in(
                &git_panel,
                window,
                |this, _, event, window, cx| match event {
                    GitPanelEvent::OpenFile(path) => this.open_path(path.clone(), None, window, cx),
                    GitPanelEvent::OpenConflict(path) => {
                        this.open_conflict(path.clone(), window, cx)
                    }
                },
            ),
            cx.observe_window_appearance(window, |_, window, cx| {
                let theme = Settings::get(cx).theme(window.appearance());
                cx.set_global(theme);
                window.refresh();
            }),
            // Settings changed: the theme mode may have too.
            cx.observe_global_in::<Settings>(window, |_, window, cx| {
                let theme = Settings::get(cx).theme(window.appearance());
                cx.set_global(theme);
                window.refresh();
            }),
        ];
        let mut subscriptions = subscriptions;
        if let Some(store) = LspStore::global(cx) {
            subscriptions.push(cx.subscribe(&store, |this, _, event, cx| match event {
                crate::lsp_store::LspStoreEvent::ApplyEdit { edit, encoding } => {
                    this.apply_workspace_edit(edit.clone(), *encoding, cx)
                }
            }));
        }
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.confirm_window_close(window, cx))
                .unwrap_or(true)
        });
        // The HUD shows memory, which changes without any view noticing.
        let hud_tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(visible) = cx.update(|cx| cx.global::<Perf>().hud_visible) else {
                    break;
                };
                if visible && this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            focus_handle: cx.focus_handle(),
            project,
            project_panel,
            project_search,
            search_bar,
            panes: vec![Pane::default()],
            active_pane: 0,
            recent: VecDeque::new(),
            sidebar: Some(SidebarTab::Files),
            modal: None,
            terminals: Vec::new(),
            active_terminal: 0,
            dock_open: false,
            _subscriptions: subscriptions,
            _hud_tick: hud_tick,
            git,
            git_panel,
        }
    }

    pub fn root(&self, cx: &App) -> PathBuf {
        self.project.read(cx).root().to_path_buf()
    }

    fn active_editor(&self) -> Option<&Entity<Editor>> {
        self.panes
            .get(self.active_pane)
            .and_then(Pane::active_editor)
    }

    fn all_editors(&self) -> impl Iterator<Item = &Entity<Editor>> {
        self.panes
            .iter()
            .flat_map(|p| p.tabs.iter().map(|t| &t.editor))
    }

    /// Each open document once, however many views it has.
    fn documents(&self, cx: &App) -> Vec<Entity<Document>> {
        let mut seen = HashSet::new();
        self.all_editors()
            .map(|e| e.read(cx).document().clone())
            .filter(|d| seen.insert(d.entity_id()))
            .collect()
    }

    fn document_for_path(&self, path: &Path, cx: &App) -> Option<Entity<Document>> {
        self.all_editors()
            .map(|e| e.read(cx).document())
            .find(|d| d.read(cx).path() == Some(path))
            .cloned()
    }

    fn view_count(&self, document: EntityId, cx: &App) -> usize {
        self.all_editors()
            .filter(|e| e.read(cx).document().entity_id() == document)
            .count()
    }

    // ------------------------------------------------------------ tabs

    /// Adds a tab to the active pane showing `editor`.
    fn add_tab(&mut self, editor: Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        let events = cx.subscribe_in(
            &editor,
            window,
            |this, editor, event, window, cx| match event {
                EditorEvent::TitleChanged | EditorEvent::SelectionsChanged => cx.notify(),
                EditorEvent::Edited | EditorEvent::Saved => {}
                EditorEvent::OpenLocations {
                    title,
                    locations,
                    always_list,
                } => {
                    this.open_locations(title.clone(), locations.clone(), *always_list, window, cx)
                }
                EditorEvent::RenameRequested { current } => {
                    let prompt = RenamePrompt::new(editor.clone(), current.clone());
                    let current = current.clone();
                    this.toggle_modal(window, cx, move |window, cx| {
                        let mut picker = Picker::new(prompt, window, cx);
                        picker.set_query(&current, cx);
                        picker
                    });
                }
                EditorEvent::ApplyWorkspaceEdit { edit, encoding } => {
                    this.apply_workspace_edit(edit.clone(), *encoding, cx)
                }
                EditorEvent::StageRows { rows } => this.stage_rows(editor, rows.clone(), cx),
                EditorEvent::ShowCodeActions { actions, encoding } => {
                    if actions.is_empty() {
                        return;
                    }
                    let picker = CodeActionPicker::new(editor.clone(), actions.clone(), *encoding);
                    this.toggle_modal(window, cx, move |window, cx| {
                        Picker::new(picker, window, cx)
                    });
                }
            },
        );
        // Clicking into an editor makes its pane the active one.
        let focus = cx.on_focus_in(&editor.focus_handle(cx), window, {
            let editor = editor.downgrade();
            move |this, _, cx| {
                let Some(editor) = editor.upgrade() else {
                    return;
                };
                if let Some((p, t)) = this.locate(&editor)
                    && (this.active_pane != p || this.panes[p].active != Some(t))
                {
                    this.active_pane = p;
                    this.panes[p].active = Some(t);
                    this.search_bar
                        .update(cx, |bar, cx| bar.set_editor(Some(editor.clone()), cx));
                    cx.notify();
                }
            }
        });
        let document = editor.read(cx).document().clone();
        if self.view_count(document.entity_id(), cx) == 0 {
            self.load_diff_base(&document, cx);
        }
        let pane = &mut self.panes[self.active_pane];
        pane.tabs.push(Tab {
            editor,
            _subscriptions: [events, focus],
        });
        let ix = pane.tabs.len() - 1;
        self.activate(self.active_pane, ix, window, cx);
    }

    fn locate(&self, editor: &Entity<Editor>) -> Option<(usize, usize)> {
        self.panes.iter().enumerate().find_map(|(p, pane)| {
            pane.tabs
                .iter()
                .position(|t| t.editor == *editor)
                .map(|t| (p, t))
        })
    }

    /// Adds an editor for text already in memory. Used at startup so the first
    /// frame shows the file, and after background reads.
    pub fn add_editor(
        &mut self,
        path: Option<PathBuf>,
        content: &str,
        jump: Option<Jump>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| Editor::new(path, content, cx));
        self.add_tab(editor.clone(), window, cx);
        if let Some(jump) = jump {
            apply_jump(&editor, jump, cx);
        }
    }

    pub fn open_path(
        &mut self,
        path: PathBuf,
        jump: Option<Jump>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = &self.panes[self.active_pane];
        if let Some(ix) = pane
            .tabs
            .iter()
            .position(|t| t.editor.read(cx).path(cx) == Some(path.as_path()))
        {
            let editor = pane.tabs[ix].editor.clone();
            self.activate(self.active_pane, ix, window, cx);
            if let Some(jump) = jump {
                apply_jump(&editor, jump, cx);
            }
            return;
        }
        // Open in another pane: add a second view of the same document.
        if let Some(document) = self.document_for_path(&path, cx) {
            let editor = cx.new(|cx| Editor::for_document(document, cx));
            self.add_tab(editor.clone(), window, cx);
            if let Some(jump) = jump {
                apply_jump(&editor, jump, cx);
            }
            return;
        }
        let read_path = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let content = cx
                .background_executor()
                .spawn(async move { std::fs::read(&read_path) })
                .await;
            let content = match content {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(err) => {
                    eprintln!("could not open {}: {err}", path.display());
                    return;
                }
            };
            this.update_in(cx, |this, window, cx| {
                this.add_editor(Some(path), &content, jump, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn activate(&mut self, pane: usize, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.panes.get(pane).and_then(|p| p.tabs.get(ix)) else {
            return;
        };
        let editor = tab.editor.clone();
        self.active_pane = pane;
        self.panes[pane].active = Some(ix);
        if let Some(path) = editor.read(cx).path(cx).map(Path::to_path_buf) {
            self.project_panel
                .update(cx, |p, cx| p.select_path(&path, cx));
            if let Some(rel) = self.project.read(cx).relative(&path) {
                let rel: Arc<str> = rel.to_string_lossy().replace('\\', "/").into();
                self.recent.retain(|r| *r != rel);
                self.recent.push_front(rel);
                self.recent.truncate(RECENT_LIMIT);
            }
        }
        self.search_bar
            .update(cx, |bar, cx| bar.set_editor(Some(editor.clone()), cx));
        window.focus(&editor.focus_handle(cx));
        cx.notify();
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor().cloned() {
            self.close(&editor, window, cx);
        }
    }

    /// Closes a tab. Asks first when it is the last view of a document with
    /// unsaved changes.
    fn close(&mut self, editor: &Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        let document = editor.read(cx).document().clone();
        let last_view = self.view_count(document.entity_id(), cx) == 1;
        if !last_view || !document.read(cx).is_dirty() {
            self.remove_tab(editor, window, cx);
            return;
        }
        let name = document.read(cx).title();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Save changes to {name}?"),
            Some("Your changes will be lost if you don't save them."),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        let editor = editor.clone();
        cx.spawn_in(window, async move |this, cx| match answer.await.ok() {
            Some(0) => {
                let saved = this
                    .update_in(cx, |this, window, cx| this.save_editor(&editor, window, cx))
                    .ok();
                if let Some(saved) = saved
                    && saved.await
                {
                    this.update_in(cx, |this, window, cx| this.remove_tab(&editor, window, cx))
                        .ok();
                }
            }
            Some(1) => {
                this.update_in(cx, |this, window, cx| this.remove_tab(&editor, window, cx))
                    .ok();
            }
            _ => {}
        })
        .detach();
    }

    fn remove_tab(&mut self, editor: &Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((p, ix)) = self.locate(editor) else {
            return;
        };
        self.panes[p].tabs.remove(ix);
        if self.panes[p].tabs.is_empty() && self.panes.len() > 1 {
            // An empty split closes; focus moves to its neighbour.
            self.panes.remove(p);
            let next = p.min(self.panes.len() - 1);
            let tab = self.panes[next].active.unwrap_or(0);
            self.activate(next, tab, window, cx);
            return;
        }
        let pane = &mut self.panes[p];
        if pane.tabs.is_empty() {
            pane.active = None;
            self.search_bar
                .update(cx, |bar, cx| bar.set_editor(None, cx));
            window.focus(&self.focus_handle);
            cx.notify();
            return;
        }
        let next = match pane.active {
            Some(a) if a > ix => a - 1,
            Some(a) if a == ix => ix.min(pane.tabs.len() - 1),
            Some(a) => a,
            None => 0,
        };
        self.activate(p, next, window, cx);
    }

    fn confirm_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let dirty: Vec<Entity<Document>> = self
            .documents(cx)
            .into_iter()
            .filter(|d| d.read(cx).is_dirty())
            .collect();
        if dirty.is_empty() {
            return true;
        }
        let message = if dirty.len() == 1 {
            format!("Save changes to {}?", dirty[0].read(cx).title())
        } else {
            format!("Save changes to {} files?", dirty.len())
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some("Your changes will be lost if you don't save them."),
            &["Save All", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let close = match answer.await.ok() {
                Some(0) => {
                    let Ok(tasks) = this.update_in(cx, |this, window, cx| {
                        let editors: Vec<Entity<Editor>> = dirty
                            .iter()
                            .filter_map(|d| {
                                this.all_editors()
                                    .find(|e| e.read(cx).document() == d)
                                    .cloned()
                            })
                            .collect();
                        editors
                            .iter()
                            .map(|e| this.save_editor(e, window, cx))
                            .collect::<Vec<_>>()
                    }) else {
                        return;
                    };
                    let mut all = true;
                    for task in tasks {
                        all &= task.await;
                    }
                    all
                }
                Some(1) => true,
                _ => false,
            };
            if close {
                this.update_in(cx, |this, window, cx| {
                    // Nothing left to ask about; close for real.
                    this.panes = vec![Pane::default()];
                    window.remove_window();
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        false
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        let pane = &self.panes[self.active_pane];
        if let Some(ix) = pane.active {
            let next = (ix + 1) % pane.tabs.len();
            self.activate(self.active_pane, next, window, cx);
        }
    }

    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        let pane = &self.panes[self.active_pane];
        if let Some(ix) = pane.active {
            let len = pane.tabs.len();
            self.activate(self.active_pane, (ix + len - 1) % len, window, cx);
        }
    }

    // ------------------------------------------------------------ git

    fn load_diff_base(&self, document: &Entity<Document>, cx: &mut Context<Self>) {
        let Some(path) = document.read(cx).path().map(Path::to_path_buf) else {
            return;
        };
        let load = self.git.update(cx, |g, cx| g.load_base(&path, cx));
        let document = document.downgrade();
        cx.spawn(async move |_, cx| {
            let base = load.await;
            document.update(cx, |d, cx| d.set_diff_base(base, cx)).ok();
        })
        .detach();
    }

    /// The index may have moved: refresh every document's diff base and the
    /// tree's colors.
    fn git_status_changed(&mut self, cx: &mut Context<Self>) {
        for document in self.documents(cx) {
            self.load_diff_base(&document, cx);
        }
        let tints = self.git.read(cx).tints();
        self.project_panel
            .update(cx, |p, cx| p.set_tints(tints, cx));
        cx.notify();
    }

    fn stage_rows(&mut self, editor: &Entity<Editor>, rows: Range<usize>, cx: &mut Context<Self>) {
        let doc = editor.read(cx).doc(cx);
        let (Some(path), Some(repo)) = (doc.path(), self.git.read(cx).repo()) else {
            return;
        };
        let Some(rel) = repo.relative(path) else {
            return;
        };
        let base = doc.diff_base().map(|b| b.to_string()).unwrap_or_default();
        let current = doc.text().text_for_save().replace("\r\n", "\n");
        let staged = crate::git::stage_rows(&base, &current, rows);
        self.git.update(cx, |g, cx| {
            g.run(move |repo| repo.stage_content(&rel, &staged), cx)
                .detach()
        });
    }

    fn show_git(&mut self, _: &ShowGit, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar = Some(SidebarTab::Git);
        self.git_panel.read(cx).focus_message(window, cx);
        self.git.update(cx, |g, cx| g.refresh_now(cx));
        cx.notify();
    }

    fn switch_branch(
        &mut self,
        _: &git_panel::SwitchBranch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let git = self.git.clone();
        self.toggle_modal(window, cx, move |window, cx| {
            let mut picker = Picker::new(BranchPicker::new(git), window, cx);
            BranchPicker::load(&mut picker, window, cx);
            picker
        });
    }

    fn git_in_terminal(
        &mut self,
        title: &str,
        args: &[&str],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = TerminalCommand {
            program: Some("git".into()),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: self.root(cx),
            title: Some(title.into()),
            ..Default::default()
        };
        self.spawn_terminal(command, window, cx);
    }

    fn push(&mut self, _: &git_panel::Push, window: &mut Window, cx: &mut Context<Self>) {
        // A terminal shows progress and handles credential prompts.
        self.git_in_terminal("git push", &["push", "-u", "origin", "HEAD"], window, cx);
    }

    fn pull(&mut self, _: &git_panel::Pull, window: &mut Window, cx: &mut Context<Self>) {
        self.git_in_terminal("git pull", &["pull", "--ff-only"], window, cx);
    }

    fn open_pull_request(
        &mut self,
        _: &git_panel::OpenPullRequest,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let git = self.git.read(cx);
        let (Some(repo), Some(branch)) = (git.repo().cloned(), git.status().branch.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let url = cx
                .background_executor()
                .spawn(async move { repo.remote_url() })
                .await
                .and_then(|remote| crate::git::pull_request_url(&remote, &branch));
            match url {
                Some(url) => {
                    cx.update(|cx| cx.open_url(&url)).ok();
                }
                None => {
                    this.update(cx, |this, cx| {
                        this.git.update(cx, |g, cx| {
                            g.last_error = Some("No GitHub or GitLab remote named origin".into());
                            cx.notify();
                        })
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Three panes: ours (read-only) | the file | theirs (read-only).
    fn open_conflict(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.git.read(cx).repo().cloned() else {
            return;
        };
        let Some(rel) = repo.relative(&path) else {
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        cx.spawn_in(window, async move |this, cx| {
            let (ours, theirs) = cx
                .background_executor()
                .spawn(async move {
                    (
                        repo.show(&format!(":2:{rel}")).unwrap_or_default(),
                        repo.show(&format!(":3:{rel}")).unwrap_or_default(),
                    )
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                let side = |title: String, content: &str, cx: &mut Context<Self>| {
                    let path = path.clone();
                    let doc = cx.new(|cx| Document::virtual_file(title, path, content, cx));
                    cx.new(|cx| Editor::for_document(doc, cx))
                };
                // Ours to the left of the current pane, theirs to the right.
                let ours_editor = side(format!("{name} (ours)"), &ours, cx);
                let theirs_editor = side(format!("{name} (theirs)"), &theirs, cx);
                let at = this.active_pane;
                this.panes.insert(at, Pane::default());
                this.active_pane = at;
                this.add_tab(ours_editor, window, cx);
                this.active_pane = at + 2;
                this.panes.insert(at + 2, Pane::default());
                this.add_tab(theirs_editor, window, cx);
                this.active_pane = at + 1;
                this.open_path(path.clone(), None, window, cx);
            })
            .ok();
        })
        .detach();
    }

    // ------------------------------------------------------------ terminal

    /// Opens a terminal in the bottom dock. Also used by the services panel
    /// to run a project's dev scripts.
    pub fn spawn_terminal(
        &mut self,
        command: TerminalCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Terminal>> {
        let started = match Terminal::start(command) {
            Ok(t) => t,
            Err(err) => {
                eprintln!("could not start a terminal: {err}");
                return None;
            }
        };
        let terminal = cx.new(|cx| Terminal::new(started, cx));
        let subscription = cx.subscribe_in(
            &terminal,
            window,
            |this, terminal, event, window, cx| match event {
                TerminalEvent::TitleChanged => cx.notify(),
                TerminalEvent::Exited => this.remove_terminal(terminal, window, cx),
            },
        );
        self.terminals.push((terminal.clone(), subscription));
        self.active_terminal = self.terminals.len() - 1;
        self.dock_open = true;
        window.focus(&terminal.focus_handle(cx));
        cx.notify();
        Some(terminal)
    }

    fn remove_terminal(
        &mut self,
        terminal: &Entity<Terminal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.terminals.iter().position(|(t, _)| t == terminal) else {
            return;
        };
        let was_focused = terminal.focus_handle(cx).contains_focused(window, cx);
        drop(self.terminals.remove(ix));
        if self.terminals.is_empty() {
            self.dock_open = false;
            self.active_terminal = 0;
        } else {
            self.active_terminal = self.active_terminal.min(self.terminals.len() - 1);
        }
        if was_focused {
            match (
                self.terminals.get(self.active_terminal),
                self.active_editor(),
            ) {
                (Some((t, _)), _) => window.focus(&t.focus_handle(cx)),
                (None, Some(e)) => window.focus(&e.focus_handle(cx)),
                (None, None) => window.focus(&self.focus_handle),
            }
        }
        cx.notify();
    }

    fn default_terminal(&self, cx: &App) -> TerminalCommand {
        TerminalCommand {
            cwd: self.root(cx),
            ..Default::default()
        }
    }

    fn toggle_terminal(&mut self, _: &ToggleTerminal, window: &mut Window, cx: &mut Context<Self>) {
        let active = self
            .terminals
            .get(self.active_terminal)
            .map(|(t, _)| t.clone());
        match active {
            None => {
                let command = self.default_terminal(cx);
                self.spawn_terminal(command, window, cx);
            }
            Some(t) if self.dock_open && t.focus_handle(cx).contains_focused(window, cx) => {
                self.dock_open = false;
                if let Some(e) = self.active_editor() {
                    window.focus(&e.focus_handle(cx));
                }
                cx.notify();
            }
            Some(t) => {
                self.dock_open = true;
                window.focus(&t.focus_handle(cx));
                cx.notify();
            }
        }
    }

    fn new_terminal(&mut self, _: &NewTerminal, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.default_terminal(cx);
        self.spawn_terminal(command, window, cx);
    }

    fn render_dock(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tabs: Vec<_> = self
            .terminals
            .iter()
            .enumerate()
            .map(|(ix, (terminal, _))| {
                let active = ix == self.active_terminal;
                let close = terminal.clone();
                div()
                    .id(("terminal-tab", ix))
                    .h(px(24.))
                    .pl_2p5()
                    .pr_1()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(px(8.))
                    .text_size(UI_FONT_SIZE)
                    .text_color(if active { theme.fg } else { theme.fg_subtle })
                    .when(active, |d| d.bg(theme.bg_elev))
                    .hover(|d| d.text_color(theme.fg))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active_terminal = ix;
                        if let Some((t, _)) = this.terminals.get(ix) {
                            window.focus(&t.focus_handle(cx));
                        }
                        cx.notify();
                    }))
                    .child(terminal.read(cx).title())
                    .child(
                        div()
                            .id(("terminal-close", ix))
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .hover(|d| d.bg(theme.line))
                            .child("×")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.remove_terminal(&close, window, cx);
                            })),
                    )
            })
            .collect();
        div()
            .h(px(DOCK_HEIGHT))
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme.line)
            .child(
                div()
                    .h(px(32.))
                    .flex_none()
                    .px_1p5()
                    .flex()
                    .items_center()
                    .gap_1()
                    .bg(theme.bg_sunken)
                    .border_b_1()
                    .border_color(theme.line)
                    .children(tabs)
                    .child(
                        div()
                            .id("terminal-new")
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .text_color(theme.fg_subtle)
                            .hover(|d| d.bg(theme.line).text_color(theme.fg))
                            .child("+")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.new_terminal(&NewTerminal, window, cx)
                            })),
                    ),
            )
            .child(
                div().flex_1().min_h_0().children(
                    self.terminals
                        .get(self.active_terminal)
                        .map(|(t, _)| t.clone()),
                ),
            )
    }

    // ------------------------------------------------------------ language servers

    fn open_locations(
        &mut self,
        title: SharedString,
        locations: Vec<LspLocation>,
        always_list: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if locations.is_empty() {
            return;
        }
        if locations.len() == 1 && !always_list {
            let l = locations.into_iter().next().unwrap();
            let jump = Jump::Lsp {
                range: l.range,
                encoding: l.encoding,
            };
            self.open_path(l.path, Some(jump), window, cx);
            return;
        }
        let root = self.root(cx);
        let open: Vec<(PathBuf, Entity<Document>)> = self
            .documents(cx)
            .into_iter()
            .filter_map(|d| d.read(cx).path().map(|p| (p.to_path_buf(), d.clone())))
            .collect();
        let mut file_cache: std::collections::HashMap<PathBuf, Vec<String>> = Default::default();
        let picker =
            LocationPicker::new(cx.weak_entity(), title, &root, locations, |path, line| {
                if let Some((_, doc)) = open.iter().find(|(p, _)| p == path) {
                    let text = doc.read(cx).text();
                    return (line < text.line_count()).then(|| text.line_str(line).into_owned());
                }
                let lines = file_cache.entry(path.clone()).or_insert_with(|| {
                    std::fs::read_to_string(path)
                        .map(|t| t.lines().map(str::to_owned).collect())
                        .unwrap_or_default()
                });
                lines.get(line).cloned()
            });
        self.toggle_modal(window, cx, move |window, cx| {
            Picker::new(picker, window, cx)
        });
    }

    /// Applies a multi-file edit: open documents change in place (and become
    /// dirty), other files are rewritten on disk.
    fn apply_workspace_edit(
        &mut self,
        edit: lsp::types::WorkspaceEdit,
        encoding: lsp::Encoding,
        cx: &mut Context<Self>,
    ) {
        use lsp::types::{DocumentChangeOperation, DocumentChanges, OneOf};
        let mut per_file: Vec<(PathBuf, Vec<lsp::types::TextEdit>)> = Vec::new();
        for (uri, edits) in edit.changes.unwrap_or_default() {
            if let Some(path) = lsp::uri_to_path(&uri) {
                per_file.push((path, edits));
            }
        }
        let text_edits = |tde: lsp::types::TextDocumentEdit| {
            let edits = tde
                .edits
                .into_iter()
                .map(|e| match e {
                    OneOf::Left(e) => e,
                    OneOf::Right(annotated) => annotated.text_edit,
                })
                .collect::<Vec<_>>();
            lsp::uri_to_path(&tde.text_document.uri).map(|p| (p, edits))
        };
        match edit.document_changes {
            Some(DocumentChanges::Edits(edits)) => {
                per_file.extend(edits.into_iter().filter_map(text_edits))
            }
            Some(DocumentChanges::Operations(ops)) => {
                per_file.extend(ops.into_iter().filter_map(|op| match op {
                    DocumentChangeOperation::Edit(tde) => text_edits(tde),
                    DocumentChangeOperation::Op(_) => None,
                }))
            }
            None => {}
        }
        for (path, edits) in per_file {
            if let Some(document) = self.document_for_path(&path, cx) {
                document.update(cx, |d, cx| {
                    let buffer = d.text();
                    let edits = edits
                        .into_iter()
                        .map(|e| (from_range(buffer, e.range, encoding), e.new_text))
                        .collect();
                    d.apply_edits(edits, None, cx);
                });
                continue;
            }
            cx.background_executor()
                .spawn(async move {
                    let Ok(bytes) = std::fs::read(&path) else {
                        return;
                    };
                    let mut buffer = text::Buffer::new(&String::from_utf8_lossy(&bytes));
                    let edits: Vec<_> = edits
                        .into_iter()
                        .map(|e| (from_range(&buffer, e.range, encoding), e.new_text))
                        .collect();
                    buffer.edit(edits, &[], std::time::Instant::now());
                    if let Err(err) = std::fs::write(&path, buffer.text_for_save()) {
                        eprintln!("could not write {}: {err}", path.display());
                    }
                })
                .detach();
        }
    }

    // ------------------------------------------------------------ splits

    fn split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        let document = self.active_editor().map(|e| e.read(cx).document().clone());
        let at = self.active_pane + 1;
        self.panes.insert(at, Pane::default());
        self.active_pane = at;
        match document {
            Some(document) => {
                let editor = cx.new(|cx| Editor::for_document(document, cx));
                self.add_tab(editor, window, cx);
            }
            None => cx.notify(),
        }
    }

    fn focus_pane(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.panes.len() as isize;
        let next = (self.active_pane as isize + delta).rem_euclid(len) as usize;
        match self.panes[next].active {
            Some(tab) => self.activate(next, tab, window, cx),
            None => {
                self.active_pane = next;
                cx.notify();
            }
        }
    }

    fn focus_next_pane(&mut self, _: &FocusNextPane, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_pane(1, window, cx);
    }

    fn focus_prev_pane(&mut self, _: &FocusPrevPane, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_pane(-1, window, cx);
    }

    // ------------------------------------------------------------ files

    fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        self.add_editor(None, "", None, window, cx);
    }

    /// Saves, asking for a path first when the document has none.
    fn save_editor(
        &mut self,
        editor: &Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        if editor.read(cx).path(cx).is_some() {
            return editor.update(cx, |e, cx| e.save(cx));
        }
        self.save_as_editor(editor.clone(), window, cx)
    }

    fn save_as_editor(
        &mut self,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let document = editor.read(cx).document().clone();
        let current = document.read(cx).path().map(Path::to_path_buf);
        let dir = current
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root(cx));
        let suggested = current
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let answer = cx.prompt_for_new_path(&dir, suggested.as_deref());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = answer.await else {
                return false;
            };
            let Ok(save) = document.update(cx, |d, cx| {
                d.set_path(path.clone(), cx);
                d.save(cx)
            }) else {
                return false;
            };
            let saved = save.await;
            this.update(cx, |this, cx| {
                this.project_panel.update(cx, |p, cx| {
                    p.refresh(cx);
                    p.reveal(&path, cx);
                });
            })
            .ok();
            saved
        })
    }

    fn save_untitled(&mut self, _: &editor::Save, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor().cloned() {
            self.save_editor(&editor, window, cx).detach();
        }
    }

    fn save_as(&mut self, _: &SaveAs, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor().cloned() {
            self.save_as_editor(editor, window, cx).detach();
        }
    }

    fn open_config_file(
        &mut self,
        path: PathBuf,
        default: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(err) = std::fs::write(&path, default) {
                eprintln!("could not create {}: {err}", path.display());
                return;
            }
        }
        self.open_path(path, None, window, cx);
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        let default = settings::default_settings_file();
        self.open_config_file(settings::settings_path(), default, window, cx);
    }

    fn open_keymap(&mut self, _: &OpenKeymap, window: &mut Window, cx: &mut Context<Self>) {
        let default = settings::DEFAULT_KEYMAP.to_string();
        self.open_config_file(settings::keymap_path(), default, window, cx);
    }

    fn open_folder(&mut self, _: &OpenFolder, _: &mut Window, cx: &mut Context<Self>) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = answer.await
                && let Some(root) = paths.into_iter().next()
            {
                cx.update(|cx| crate::open_workspace_window(root, None, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Files changed on disk: reload clean documents, refresh the tree.
    fn files_changed(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for document in self.documents(cx) {
            let (path, dirty) = {
                let d = document.read(cx);
                (d.path().map(Path::to_path_buf), d.is_dirty())
            };
            let Some(path) = path.filter(|p| paths.contains(p)) else {
                continue;
            };
            if dirty {
                continue;
            }
            cx.spawn(async move |_, cx| {
                let content = cx
                    .background_executor()
                    .spawn(async move { std::fs::read(&path) })
                    .await;
                if let Ok(bytes) = content {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    document
                        .update(cx, |d, cx| {
                            if !d.is_dirty() {
                                d.reload(&text, cx);
                            }
                        })
                        .ok();
                }
            })
            .detach();
        }
        self.project_panel.update(cx, |p, cx| p.refresh(cx));
    }

    fn follow_rename(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for document in self.documents(cx) {
            let path = document.read(cx).path().map(Path::to_path_buf);
            if let Some(path) = path
                && let Ok(rest) = path.strip_prefix(from)
            {
                let new = if rest.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(rest)
                };
                document.update(cx, |d, cx| d.set_path(new, cx));
            }
        }
        cx.notify();
    }

    // ------------------------------------------------------------ modals

    fn toggle_modal<V: ManagedView>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    ) {
        if let Some(modal) = &self.modal
            && modal.type_id == TypeId::of::<V>()
        {
            self.dismiss_modal(window, cx);
            return;
        }
        let previous_focus = match self.modal.take() {
            Some(old) => old.previous_focus,
            None => window.focused(cx),
        };
        let view = cx.new(|cx| build(window, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, _, _: &DismissEvent, window, cx| {
                this.dismiss_modal(window, cx);
            });
        let focus = view.focus_handle(cx);
        window.focus(&focus);
        self.modal = Some(Modal {
            view: view.into(),
            type_id: TypeId::of::<V>(),
            focus,
            previous_focus,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn dismiss_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        // Only take focus back if the modal still had it; a command may have
        // moved focus on purpose.
        let still_focused =
            modal.focus.contains_focused(window, cx) || window.focused(cx).is_none();
        if still_focused && let Some(previous) = modal.previous_focus {
            window.focus(&previous);
        }
        cx.notify();
    }

    fn toggle_command_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = CommandPalette::new(window, cx);
        self.toggle_modal(window, cx, move |window, cx| {
            Picker::new(palette, window, cx)
        });
    }

    fn toggle_file_finder(
        &mut self,
        _: &ToggleFileFinder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (root, files) = {
            let p = self.project.read(cx);
            (p.root().to_path_buf(), p.files())
        };
        // The current file goes last: the finder is usually for switching away.
        let recent: Vec<Arc<str>> = self
            .recent
            .iter()
            .skip(1)
            .chain(self.recent.iter().take(1))
            .cloned()
            .collect();
        let finder = FileFinder::new(cx.weak_entity(), root, files, recent);
        self.toggle_modal(window, cx, move |window, cx| {
            Picker::new(finder, window, cx)
        });
    }

    fn go_to_line(&mut self, _: &GoToLine, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor().cloned() else {
            return;
        };
        let delegate = GoToLineDelegate::new(editor, cx);
        self.toggle_modal(window, cx, move |window, cx| {
            Picker::new(delegate, window, cx)
        });
    }

    // ------------------------------------------------------------ search

    fn find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        self.deploy_search(false, window, cx);
    }

    fn find_replace(&mut self, _: &FindReplace, window: &mut Window, cx: &mut Context<Self>) {
        self.deploy_search(true, window, cx);
    }

    fn deploy_search(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor().cloned() else {
            return;
        };
        self.search_bar
            .update(cx, |bar, cx| bar.deploy(editor, replace, window, cx));
        cx.notify();
    }

    fn find_next(&mut self, _: &FindNext, window: &mut Window, cx: &mut Context<Self>) {
        self.search_bar.update(cx, |bar, cx| {
            bar.find_next(&buffer_search::FindNext, window, cx)
        });
    }

    fn find_prev(&mut self, _: &FindPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.search_bar.update(cx, |bar, cx| {
            bar.find_prev(&buffer_search::FindPrev, window, cx)
        });
    }

    fn show_files(&mut self, _: &ShowFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar = Some(SidebarTab::Files);
        window.focus(&self.project_panel.focus_handle(cx));
        cx.notify();
    }

    fn show_search(&mut self, _: &ShowSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar = Some(SidebarTab::Search);
        let selected = self
            .active_editor()
            .and_then(|e| e.read(cx).selected_text(cx));
        self.project_search
            .update(cx, |s, cx| s.focus_query(selected, window, cx));
        cx.notify();
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar = match self.sidebar {
            Some(_) => None,
            None => Some(SidebarTab::Files),
        };
        cx.notify();
    }

    fn reveal_active_file(
        &mut self,
        _: &RevealActiveFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self
            .active_editor()
            .and_then(|e| e.read(cx).path(cx).map(Path::to_path_buf))
        else {
            return;
        };
        self.sidebar = Some(SidebarTab::Files);
        self.project_panel.update(cx, |p, cx| p.reveal(&path, cx));
        window.focus(&self.project_panel.focus_handle(cx));
        cx.notify();
    }

    fn toggle_hud(&mut self, _: &ToggleHud, _: &mut Window, cx: &mut Context<Self>) {
        let perf = cx.global_mut::<Perf>();
        perf.hud_visible = !perf.hud_visible;
        cx.notify();
    }

    pub fn bench_insert(&mut self, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |e, cx| e.insert(text, cx));
        }
    }

    // ------------------------------------------------------------ render

    fn render_pane(
        &self,
        p: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let pane = &self.panes[p];
        let is_active_pane = p == self.active_pane;
        let search_visible =
            is_active_pane && self.search_bar.read(cx).visible && pane.active_editor().is_some();
        let tabs: Vec<_> = pane
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let editor = tab.editor.clone();
                let doc = tab.editor.read(cx).doc(cx);
                let name: SharedString = doc.title().into();
                let dirty = doc.is_dirty();
                let active = pane.active == Some(ix);
                let close_editor = editor.clone();
                let middle_editor = editor.clone();
                div()
                    .id(("tab", p * 10_000 + ix))
                    .flex_none()
                    .h(px(26.))
                    .pl_3()
                    .pr_1p5()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(px(8.))
                    .text_size(UI_FONT_SIZE)
                    .text_color(if active && is_active_pane {
                        theme.fg
                    } else if active {
                        theme.fg_muted
                    } else {
                        theme.fg_subtle
                    })
                    .when(active, |d| {
                        d.bg(theme.bg_elev).border_1().border_color(theme.line)
                    })
                    .hover(|d| d.text_color(theme.fg))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| this.activate(p, ix, window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            this.close(&middle_editor, window, cx)
                        }),
                    )
                    .child(name)
                    .child(
                        // Unsaved state doubles as the close target, like most editors.
                        div()
                            .id(("close", p * 10_000 + ix))
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .text_color(theme.fg_subtle)
                            .hover(|d| d.bg(theme.line).text_color(theme.fg))
                            .child(if dirty { "●" } else { "×" })
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close(&close_editor, window, cx)
                            })),
                    )
            })
            .collect();
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .when(p > 0, |d| d.border_l_1().border_color(theme.line))
            .child(
                div()
                    .id(("tab-bar", p))
                    .h(px(TAB_BAR_HEIGHT))
                    .flex_none()
                    .px_1p5()
                    .flex()
                    .items_center()
                    .gap_1()
                    .overflow_x_scroll()
                    .border_b_1()
                    .border_color(theme.line)
                    .bg(theme.bg_sunken)
                    .children(tabs),
            )
            .when(search_visible, |d| d.child(self.search_bar.clone()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .map(|d| match pane.active_editor() {
                        Some(editor) => d.child(editor.clone()),
                        None => d.child(self.render_empty(window, cx)),
                    }),
            )
    }

    fn render_sidebar(&self, tab: SidebarTab, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tab_button = |id: &'static str, label: &'static str, this_tab: SidebarTab| {
            let active = tab == this_tab;
            div()
                .id(id)
                .h(px(24.))
                .px_2()
                .flex()
                .items_center()
                .rounded(px(8.))
                .text_size(UI_FONT_SIZE)
                .text_color(if active { theme.fg } else { theme.fg_subtle })
                .when(active, |d| d.bg(theme.bg_elev))
                .hover(|d| d.text_color(theme.fg))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| match this_tab {
                    SidebarTab::Files => this.show_files(&ShowFiles, window, cx),
                    SidebarTab::Search => this.show_search(&ShowSearch, window, cx),
                    SidebarTab::Git => this.show_git(&ShowGit, window, cx),
                }))
        };
        div()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.line)
            .bg(theme.bg_sunken)
            .child(
                div()
                    .flex_none()
                    .h(px(TAB_BAR_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.line)
                    .child(tab_button("sidebar-files", "Files", SidebarTab::Files))
                    .child(tab_button("sidebar-search", "Search", SidebarTab::Search))
                    .child(tab_button("sidebar-git", "Git", SidebarTab::Git)),
            )
            .child(div().flex_1().min_h_0().pt_1().map(|d| match tab {
                SidebarTab::Files => d.child(self.project_panel.clone()),
                SidebarTab::Search => d.child(self.project_search.clone()),
                SidebarTab::Git => d.child(self.git_panel.clone()),
            }))
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut left: Vec<String> = Vec::new();
        if let Some(editor) = self.active_editor() {
            let e = editor.read(cx);
            let doc = e.doc(cx);
            let (line, col, cursors) = e.cursor_position(cx);
            left.push(format!("Ln {line}, Col {col}"));
            if cursors > 1 {
                left.push(format!("{cursors} cursors"));
            }
            if doc.is_read_only() {
                left.push("Read-only".into());
            }
            left.push(doc.indent_label().to_string());
            left.push(doc.language_name().unwrap_or("Plain text").to_string());
            let (errors, warnings) = crate::editor_lsp::diagnostic_counts(doc);
            if let Some(text) = crate::editor_lsp::status_text(errors, warnings) {
                left.push(text.to_string());
            }
        }
        if let Some(status) = LspStore::global(cx).and_then(|s| s.read(cx).status().cloned()) {
            left.push(status.to_string());
        }
        if self.project.read(cx).is_scanning() {
            left.push("Indexing files...".into());
        }
        let config_error = cx
            .try_global::<settings::ConfigErrors>()
            .and_then(|e| e.0.first().cloned());

        let perf = cx.global::<Perf>();
        let mut right: Vec<String> = Vec::new();
        if perf.hud_visible {
            if let Some((p50, p99)) = perf.input_p50_p99() {
                right.push(format!("input {} ms, p99 {}", perf::ms(p50), perf::ms(p99)));
            }
            if let Some((p50, _)) = perf.frame_p50_p99() {
                right.push(format!("frame {} ms", perf::ms(p50)));
            }
            if let Some(bytes) = perf::resident_memory() {
                right.push(format!("{} MB", bytes / (1024 * 1024)));
            }
            if let Some(start) = perf.first_frame {
                right.push(format!("start {} ms", perf::ms(start)));
            }
        }

        let item = |text: String| div().child(text);
        div()
            .h(px(STATUS_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .border_t_1()
            .border_color(theme.line)
            .bg(theme.bg_sunken)
            .text_size(px(11.5))
            .text_color(theme.fg_subtle)
            .child(
                div()
                    .flex()
                    .gap_4()
                    .min_w_0()
                    .children(left.into_iter().map(item))
                    .children(
                        config_error.map(|e| div().truncate().text_color(theme.error).child(e)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap_4()
                    .children(right.into_iter().map(item)),
            )
    }

    fn render_empty(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let hint = |label: &'static str, action: &dyn gpui::Action| {
            let keys = window
                .highest_precedence_binding_for_action(action)
                .map(|b| crate::picker::format_binding(&b))
                .unwrap_or_default();
            div()
                .flex()
                .justify_between()
                .gap_8()
                .w(px(280.))
                .text_size(UI_FONT_SIZE)
                .child(div().text_color(theme.fg_muted).child(label))
                .child(div().text_color(theme.fg_subtle).child(keys))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2p5()
            .bg(theme.bg)
            .child(hint("Go to file", &ToggleFileFinder))
            .child(hint("Run a command", &ToggleCommandPalette))
            .child(hint("Search in project", &ShowSearch))
            .child(hint("New file", &NewFile))
            .child(hint("Open folder", &OpenFolder))
    }
}

fn apply_jump(editor: &Entity<Editor>, jump: Jump, cx: &mut App) {
    editor.update(cx, |e, cx| match jump {
        Jump::Point { row, column } => e.go_to_point(row, column, cx),
        Jump::RowColumns { row, columns } => e.select_in_row(row, columns, cx),
        Jump::Lsp { range, encoding } => {
            let r = from_range(e.buf(cx), range, encoding);
            e.select_range(r, cx);
        }
    });
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let root = self.root(cx);
        let project_name = root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let title = self
            .active_editor()
            .and_then(|e| e.read(cx).path(cx).map(|p| p.display().to_string()))
            .unwrap_or_else(|| root.display().to_string());
        window.set_window_title(&title);
        let panes: Vec<_> = (0..self.panes.len())
            .map(|p| self.render_pane(p, window, cx).into_any_element())
            .collect();

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::toggle_hud))
            .on_action(cx.listener(Self::toggle_command_palette))
            .on_action(cx.listener(Self::toggle_file_finder))
            .on_action(cx.listener(Self::go_to_line))
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(Self::find_replace))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_prev))
            .on_action(cx.listener(Self::show_files))
            .on_action(cx.listener(Self::show_search))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::reveal_active_file))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::save_untitled))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::open_keymap))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::focus_next_pane))
            .on_action(cx.listener(Self::focus_prev_pane))
            .on_action(cx.listener(Self::toggle_terminal))
            .on_action(cx.listener(Self::show_git))
            .on_action(cx.listener(Self::switch_branch))
            .on_action(cx.listener(Self::push))
            .on_action(cx.listener(Self::pull))
            .on_action(cx.listener(Self::open_pull_request))
            .on_action(cx.listener(Self::new_terminal))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .font_family(UI_FONT)
            .text_color(theme.fg)
            .child(
                div()
                    .id("titlebar")
                    .h(px(TITLEBAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    // Room for the macOS traffic lights.
                    .pl(px(if cfg!(target_os = "macos") { 84. } else { 12. }))
                    .pr_3()
                    .border_b_1()
                    .border_color(theme.line)
                    .bg(theme.bg_sunken)
                    .text_size(UI_FONT_SIZE)
                    .text_color(theme.fg_muted)
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(MouseButton::Left, |event, window, _| {
                        if event.click_count == 2 {
                            window.titlebar_double_click();
                        } else {
                            window.start_window_move();
                        }
                    })
                    .child(project_name),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .children(self.sidebar.map(|tab| self.render_sidebar(tab, cx)))
                    .children(panes),
            )
            .when(self.dock_open && !self.terminals.is_empty(), |d| {
                d.child(self.render_dock(cx))
            })
            .child(self.render_status(cx))
            .children(self.modal.as_ref().map(|modal| {
                deferred(
                    div()
                        .absolute()
                        .top(px(TITLEBAR_HEIGHT + 24.))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .occlude()
                                .on_mouse_down_out(
                                    cx.listener(|this, _, window, cx| {
                                        this.dismiss_modal(window, cx)
                                    }),
                                )
                                .child(modal.view.clone()),
                        ),
                )
                .with_priority(1)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use std::time::Instant;

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("solder-ws-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/util")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    helper();\n}\n").unwrap();
        std::fs::write(
            dir.join("src/util/strings.rs"),
            "pub fn helper() {}\n// needle here\n",
        )
        .unwrap();
        std::fs::write(dir.join("README.md"), "A needle in the docs.\n").unwrap();
        dir.canonicalize().unwrap()
    }

    fn setup(
        cx: &mut TestAppContext,
        root: PathBuf,
    ) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(Perf::new(Instant::now()));
            cx.set_global(crate::theme::Theme::dark());
            cx.set_global(Settings::default());
            settings::bind_defaults(cx);
            crate::lsp_store::init(cx);
        });
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(root, window, cx));
        cx.update(|window, cx| window.focus(&workspace.focus_handle(cx)));
        cx.run_until_parked();
        (workspace, cx)
    }

    fn active_text(ws: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
        cx.read(|cx| ws.read(cx).active_editor().unwrap().read(cx).text(cx))
    }

    fn active_path(ws: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<PathBuf> {
        cx.read(|cx| {
            ws.read(cx)
                .active_editor()
                .and_then(|e| e.read(cx).path(cx).map(Path::to_path_buf))
        })
    }

    #[gpui::test]
    fn file_finder_opens_fuzzy_match(cx: &mut TestAppContext) {
        let root = fixture("finder");
        let (ws, cx) = setup(cx, root.clone());
        cx.simulate_keystrokes("secondary-p");
        cx.simulate_input("utstr");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(active_path(&ws, cx), Some(root.join("src/util/strings.rs")));
        assert!(cx.read(|cx| ws.read(cx).modal.is_none()));
    }

    #[gpui::test]
    fn command_palette_runs_editor_action(cx: &mut TestAppContext) {
        let root = fixture("palette");
        let (ws, cx) = setup(cx, root.clone());
        ws.update_in(cx, |w, window, cx| {
            w.add_editor(Some(root.join("x.txt")), "one\ntwo", None, window, cx)
        });
        cx.simulate_keystrokes("secondary-shift-p");
        cx.simulate_input("duplicate line");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(active_text(&ws, cx), "one\none\ntwo");
    }

    #[gpui::test]
    fn go_to_line_moves_cursor(cx: &mut TestAppContext) {
        let root = fixture("goto");
        let (ws, cx) = setup(cx, root.clone());
        ws.update_in(cx, |w, window, cx| {
            w.add_editor(Some(root.join("x.txt")), "a\nb\nc\nd", None, window, cx)
        });
        cx.simulate_keystrokes("ctrl-g");
        cx.simulate_input("3");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let pos = cx.read(|cx| {
            ws.read(cx)
                .active_editor()
                .unwrap()
                .read(cx)
                .cursor_position(cx)
        });
        assert_eq!(pos, (3, 1, 1));
        // Focus went back to the editor: typing lands in the file.
        cx.simulate_input("!");
        assert_eq!(active_text(&ws, cx), "a\nb\n!c\nd");
    }

    #[gpui::test]
    fn find_and_replace(cx: &mut TestAppContext) {
        let root = fixture("find");
        let (ws, cx) = setup(cx, root.clone());
        ws.update_in(cx, |w, window, cx| {
            w.add_editor(
                Some(root.join("x.txt")),
                "foo bar Foo baz foo",
                None,
                window,
                cx,
            )
        });
        cx.simulate_keystrokes("secondary-f");
        cx.simulate_input("foo");
        cx.run_until_parked();
        let matches = cx.read(|cx| ws.read(cx).search_bar.read(cx).matches.len());
        assert_eq!(matches, 3);
        cx.simulate_keystrokes("alt-secondary-c");
        cx.run_until_parked();
        let matches = cx.read(|cx| ws.read(cx).search_bar.read(cx).matches.len());
        assert_eq!(matches, 2);
        cx.simulate_keystrokes("tab");
        cx.simulate_input("qux");
        cx.simulate_keystrokes("secondary-enter");
        cx.run_until_parked();
        assert_eq!(active_text(&ws, cx), "qux bar Foo baz qux");
        cx.simulate_keystrokes("escape");
        assert!(!cx.read(|cx| ws.read(cx).search_bar.read(cx).visible));
    }

    #[gpui::test]
    fn project_search_finds_matches(cx: &mut TestAppContext) {
        let root = fixture("search");
        let (ws, cx) = setup(cx, root.clone());
        cx.simulate_keystrokes("secondary-shift-f");
        cx.simulate_input("needle");
        cx.executor().advance_clock(Duration::from_millis(400));
        cx.run_until_parked();
        let counts = cx.read(|cx| ws.read(cx).project_search.read(cx).result_count());
        assert_eq!(counts, (2, 2));
    }

    #[gpui::test]
    fn tree_creates_and_renames_files(cx: &mut TestAppContext) {
        let root = fixture("tree");
        let (ws, cx) = setup(cx, root.clone());
        let panel = cx.read(|cx| ws.read(cx).project_panel.clone());
        // New file at the root: type a name, confirm, it opens.
        cx.simulate_keystrokes("secondary-shift-e");
        cx.dispatch_action(crate::project_panel::NewFile);
        cx.simulate_input("notes.md");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let created = root.join("notes.md");
        assert!(created.exists());
        assert_eq!(active_path(&ws, cx), Some(created.clone()));
        // Rename it from the tree; the open tab follows.
        panel.update_in(cx, |p, window, cx| {
            p.select_path(&created, cx);
            window.focus(&p.focus_handle(cx));
        });
        cx.simulate_keystrokes("f2");
        cx.simulate_keystrokes("secondary-a");
        cx.simulate_input("todo.md");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!created.exists());
        assert!(root.join("todo.md").exists());
        assert_eq!(active_path(&ws, cx), Some(root.join("todo.md")));
    }

    #[gpui::test]
    fn split_views_share_one_document(cx: &mut TestAppContext) {
        let root = fixture("split");
        let (ws, cx) = setup(cx, root.clone());
        ws.update_in(cx, |w, window, cx| {
            w.add_editor(Some(root.join("x.txt")), "abc", None, window, cx)
        });
        let left = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        cx.simulate_keystrokes("secondary-right");
        cx.simulate_keystrokes("secondary-\\");
        let right = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        assert_ne!(left, right);
        assert_eq!(cx.read(|cx| ws.read(cx).panes.len()), 2);
        // Type at the start in the right view; the left view's cursor (at the
        // end) moves with the text, and both show the same contents.
        cx.simulate_keystrokes("secondary-left");
        cx.simulate_input(">> ");
        let (left_text, left_cursor) =
            cx.read(|cx| (left.read(cx).text(cx), left.read(cx).newest_range()));
        assert_eq!(left_text, ">> abc");
        assert_eq!(left_cursor, 6..6);
        // Closing one view of a dirty document does not prompt.
        cx.simulate_keystrokes("secondary-w");
        assert_eq!(cx.read(|cx| ws.read(cx).panes.len()), 1);
        assert_eq!(active_text(&ws, cx), ">> abc");
    }

    /// Runs the real client against `tests/fixtures/mock_lsp.py` over stdio.
    #[gpui::test]
    fn language_server_features(cx: &mut TestAppContext) {
        let root = fixture("lsp");
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        let file = root.join("src/main.rs");
        std::fs::write(&file, "fn helper() {}\n// TODO fix\n").unwrap();
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_lsp.py");
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        cx.update(|_, cx| {
            let mut settings = Settings::default();
            settings.language_servers.insert(
                "rust-analyzer".into(),
                settings::ServerOverride {
                    command: Some("python3".into()),
                    args: Some(vec![script.display().to_string()]),
                    ..Default::default()
                },
            );
            cx.set_global(settings);
        });
        ws.update_in(cx, |w, window, cx| {
            let content = std::fs::read_to_string(&file).unwrap();
            w.add_editor(Some(file.clone()), &content, None, window, cx)
        });
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let wait = |cx: &mut VisualTestContext, what: &str, f: &dyn Fn(&App) -> bool| {
            for _ in 0..400 {
                cx.run_until_parked();
                if cx.read(|cx| f(cx)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("timed out waiting for {what}");
        };

        // Diagnostics arrive after didOpen and point at "TODO".
        wait(cx, "diagnostics", &|cx| {
            !editor.read(cx).doc(cx).diagnostics().is_empty()
        });
        let diag = cx.read(|cx| editor.read(cx).doc(cx).diagnostics()[0].clone());
        assert_eq!(diag.range, 18..22);
        assert_eq!(diag.severity, crate::document::Severity::Warning);

        // Incremental sync: a new "boom" becomes an error.
        cx.simulate_keystrokes("secondary-down");
        cx.simulate_input("boom");
        wait(cx, "error diagnostic", &|cx| {
            editor
                .read(cx)
                .doc(cx)
                .diagnostics()
                .iter()
                .any(|d| d.severity == crate::document::Severity::Error)
        });

        // Completion: typing a word opens the menu, Enter expands the snippet.
        cx.simulate_keystrokes("enter");
        cx.simulate_input("pri");
        wait(cx, "completions", &|cx| {
            editor.read(cx).completion.is_some()
        });
        // The menu refilters as you type, without another request.
        cx.simulate_input("ntl");
        let top = cx.read(|cx| {
            editor
                .read(cx)
                .completion
                .as_ref()
                .unwrap()
                .selected_item()
                .unwrap()
                .label
                .clone()
        });
        assert_eq!(top, "println");
        cx.simulate_keystrokes("enter");
        let text = cx.read(|cx| editor.read(cx).text(cx));
        assert!(text.ends_with("boom\nprintln!(\"\")"), "{text:?}");
        let cursor = cx.read(|cx| editor.read(cx).newest_range());
        assert_eq!(cursor.start, text.len() - 2);

        // Go to definition selects the symbol.
        cx.simulate_keystrokes("secondary-up f12");
        wait(cx, "definition", &|cx| {
            editor.read(cx).newest_range() == (3..9)
        });

        // Rename through the prompt rewrites every occurrence.
        cx.simulate_keystrokes("f2");
        cx.simulate_input("assist");
        cx.simulate_keystrokes("enter");
        wait(cx, "rename", &|cx| {
            editor.read(cx).text(cx).starts_with("fn assist()")
        });

        // Formatting strips trailing spaces through a server edit.
        cx.simulate_keystrokes("secondary-right");
        cx.simulate_input("   ");
        cx.simulate_keystrokes("shift-alt-f");
        wait(cx, "formatting", &|cx| {
            editor.read(cx).text(cx).starts_with("fn assist() {}\n")
        });

        // Code actions: one resolved lazily, one run as a server command that
        // edits the file through workspace/applyEdit.
        cx.simulate_keystrokes("secondary-.");
        wait(cx, "code actions", &|cx| ws.read(cx).modal.is_some());
        cx.simulate_keystrokes("enter");
        wait(cx, "resolved edit", &|cx| {
            editor.read(cx).text(cx).starts_with("// header\n")
        });
        cx.simulate_keystrokes("secondary-.");
        wait(cx, "code actions", &|cx| ws.read(cx).modal.is_some());
        cx.simulate_input("touch");
        cx.simulate_keystrokes("enter");
        wait(cx, "server edit", &|cx| {
            editor.read(cx).text(cx).starts_with("// touched\n")
        });

        // Signature help follows the argument under the cursor.
        cx.simulate_keystrokes("secondary-down enter");
        cx.simulate_input("assist(");
        let active = |cx: &App| {
            editor
                .read(cx)
                .signature
                .as_ref()
                .and_then(|s| s.active.clone().map(|r| s.label[r].to_string()))
        };
        wait(cx, "signature", &|cx| {
            active(cx).as_deref() == Some("a: i32")
        });
        cx.simulate_input("1,");
        wait(cx, "second parameter", &|cx| {
            active(cx).as_deref() == Some("b: i32")
        });
        cx.simulate_keystrokes("escape");
        assert!(cx.read(|cx| editor.read(cx).signature.is_none()));
    }

    /// A real PTY: type a command, read its output off the grid, exit.
    #[gpui::test]
    fn terminal_runs_commands(cx: &mut TestAppContext) {
        let root = fixture("terminal");
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        let terminal = ws
            .update_in(cx, |w, window, cx| {
                let command = TerminalCommand {
                    program: Some("/bin/sh".into()),
                    cwd: root.clone(),
                    ..Default::default()
                };
                w.spawn_terminal(command, window, cx)
            })
            .expect("terminal starts");
        let wait = |cx: &mut VisualTestContext, what: &str, f: &dyn Fn(&App) -> bool| {
            for _ in 0..500 {
                cx.run_until_parked();
                if cx.read(|cx| f(cx)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("timed out waiting for {what}");
        };
        cx.simulate_input("echo solder-$((40+2))");
        cx.simulate_keystrokes("enter");
        wait(cx, "command output", &|cx| {
            terminal
                .read(cx)
                .visible_text()
                .iter()
                .any(|l| l == "solder-42")
        });
        cx.simulate_input("exit");
        cx.simulate_keystrokes("enter");
        wait(cx, "terminal to close", &|cx| {
            ws.read(cx).terminals.is_empty()
        });
        assert!(!cx.read(|cx| ws.read(cx).dock_open));
    }

    fn git_fixture(name: &str) -> PathBuf {
        let dir = fixture(name);
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        dir
    }

    fn wait_for(cx: &mut VisualTestContext, what: &str, f: &dyn Fn(&App) -> bool) {
        for _ in 0..500 {
            // Debounce timers run on the test executor's virtual clock.
            cx.executor().advance_clock(Duration::from_millis(50));
            cx.run_until_parked();
            if cx.read(|cx| f(cx)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    #[gpui::test]
    fn git_stage_lines_revert_and_commit(cx: &mut TestAppContext) {
        let root = git_fixture("git-flow");
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        let file = root.join("a.txt");
        ws.update_in(cx, |w, window, cx| {
            w.add_editor(Some(file.clone()), "one\ntwo\n", None, window, cx)
        });
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        wait_for(cx, "diff base", &|cx| {
            editor.read(cx).doc(cx).diff_base().is_some()
        });

        // Two new lines in the middle show up as one inserted hunk.
        cx.simulate_keystrokes("down");
        cx.simulate_input("mid1\nmid2\n");
        wait_for(cx, "hunk", &|cx| {
            editor
                .read(cx)
                .doc(cx)
                .hunks()
                .first()
                .map(|h| (h.old.clone(), h.new.clone()))
                == Some((1..1, 1..3))
        });

        // Stage only "mid2".
        cx.simulate_keystrokes("up");
        cx.simulate_keystrokes("secondary-alt-y");
        let repo = crate::git::Repo::discover(&root).unwrap();
        wait_for(cx, "staged line", &|_| {
            repo.index_text("a.txt").as_deref() == Some("one\nmid2\ntwo\n")
        });
        // The new base arrives and only "mid1" is left unstaged.
        wait_for(cx, "rebased hunks", &|cx| {
            editor
                .read(cx)
                .doc(cx)
                .hunks()
                .iter()
                .map(|h| (h.new.start, h.new.end))
                .collect::<Vec<_>>()
                == [(1, 2)]
        });

        // Revert "mid1" back to the staged text.
        cx.simulate_keystrokes("up");
        cx.simulate_keystrokes("secondary-alt-z");
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), "one\nmid2\ntwo\n");

        // Save, then commit from the panel.
        cx.simulate_keystrokes("secondary-s");
        cx.simulate_keystrokes("ctrl-shift-g");
        cx.simulate_input("Add mid2");
        cx.simulate_keystrokes("secondary-enter");
        wait_for(cx, "clean status", &|cx| {
            let g = ws.read(cx).git.read(cx);
            g.status().files.is_empty() && g.last_error.is_none()
        });
        let log = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["log", "-1", "--format=%s"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "Add mid2");
    }

    #[gpui::test]
    fn git_conflict_three_way_and_accept(cx: &mut TestAppContext) {
        let root = git_fixture("git-conflict");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap()
        };
        git(&["switch", "-q", "-c", "theirs"]);
        std::fs::write(root.join("a.txt"), "one\nTHEIRS\n").unwrap();
        git(&["commit", "-qam", "theirs"]);
        git(&["switch", "-q", "main"]);
        std::fs::write(root.join("a.txt"), "one\nOURS\n").unwrap();
        git(&["commit", "-qam", "ours"]);
        git(&["merge", "-q", "theirs"]);

        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        wait_for(cx, "conflict status", &|cx| {
            ws.read(cx)
                .git
                .read(cx)
                .status()
                .files
                .iter()
                .any(|f| f.conflicted)
        });
        let file = root.join("a.txt");
        ws.update_in(cx, |w, window, cx| {
            w.open_conflict(file.clone(), window, cx)
        });
        wait_for(cx, "three panes", &|cx| {
            let w = ws.read(cx);
            w.panes.len() == 3 && w.panes.iter().all(|p| p.active_editor().is_some())
        });
        let titles: Vec<String> = cx.read(|cx| {
            ws.read(cx)
                .panes
                .iter()
                .map(|p| p.active_editor().unwrap().read(cx).doc(cx).title())
                .collect()
        });
        assert_eq!(titles, ["a.txt (ours)", "a.txt", "a.txt (theirs)"]);
        // The sides are read-only.
        let ours = cx.read(|cx| ws.read(cx).panes[0].active_editor().unwrap().clone());
        ours.update_in(cx, |e, window, cx| window.focus(&e.focus_handle(cx)));
        cx.simulate_input("x");
        assert_eq!(cx.read(|cx| ours.read(cx).text(cx)), "one\nOURS\n");

        // In the file, take theirs.
        let result = cx.read(|cx| ws.read(cx).panes[1].active_editor().unwrap().clone());
        wait_for(cx, "conflict regions", &|cx| {
            !result.read(cx).doc(cx).conflicts().is_empty()
        });
        result.update_in(cx, |e, window, cx| {
            window.focus(&e.focus_handle(cx));
            e.go_to_point(2, 0, cx);
        });
        cx.simulate_keystrokes("secondary-k 2");
        assert_eq!(cx.read(|cx| result.read(cx).text(cx)), "one\nTHEIRS\n");
    }

    #[gpui::test]
    fn new_file_is_untitled_until_saved(cx: &mut TestAppContext) {
        let root = fixture("untitled");
        let (ws, cx) = setup(cx, root);
        cx.simulate_keystrokes("secondary-n");
        cx.simulate_input("hello");
        assert_eq!(active_text(&ws, cx), "hello");
        assert_eq!(active_path(&ws, cx), None);
    }
}
