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
    database::{self, DatabaseEvent, DatabaseStore},
    database_panel::{
        ConnectionPicker, DatabasePanel, DatabasePanelEvent, NewConnection, NewConnectionPrompt,
    },
    document::Document,
    editor::{self, Editor, EditorEvent},
    editor_lsp::LspLocation,
    file_diff::{DiffModel, FileDiff, FileDiffEvent},
    file_finder::FileFinder,
    git::DiffScope,
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
    results::ResultsView,
    services::{self, ServiceSpec},
    services_panel::{RunStack, ServicesEvent, ServicesPanel, StopAll},
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
        ShowServices,
        ShowDatabase,
        ShowApi,
        ShowAi,
        ToggleChat,
        RunStatement,
        SelectConnection,
        ShowFileDiff,
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
        KeyBinding::new("f3", FindNext, None),
        KeyBinding::new("shift-f3", FindPrev, None),
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
        KeyBinding::new("secondary-shift-s", ShowServices, None),
        KeyBinding::new("ctrl-shift-d", ShowDatabase, None),
        KeyBinding::new("ctrl-shift-h", ShowApi, None),
        KeyBinding::new("ctrl-shift-a", ShowAi, None),
        KeyBinding::new("secondary-shift-l", ToggleChat, None),
        KeyBinding::new(
            "secondary-enter",
            RunStatement,
            Some("Editor && mode == full"),
        ),
        KeyBinding::new("secondary-alt-d", ShowFileDiff, None),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-g", FindNext, None),
        KeyBinding::new("cmd-shift-g", FindPrev, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}

const SIDEBAR_WIDTH: f32 = 330.;
const CHAT_WIDTH: f32 = 380.;
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
    Services,
    Database,
    Api,
    Ai,
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
    services: Entity<ServicesPanel>,
    database: Entity<DatabaseStore>,
    database_panel: Entity<DatabasePanel>,
    api_panel: Entity<crate::api_panel::ApiPanel>,
    ai_panel: Entity<crate::ai_panel::AiPanel>,
    chat: Entity<crate::chat_panel::ChatPanel>,
    chat_open: bool,
    results: Entity<ResultsView>,
    /// The Results tab is in the dock (a query has run and it was not closed).
    show_results: bool,
    /// The dock shows Results rather than a terminal.
    results_active: bool,
    /// The last HTTP response, the dock's Response tab.
    response: Entity<crate::response::ResponseView>,
    show_response: bool,
    /// Scratch queries and the requests file; tests point it elsewhere.
    scratch_dir: PathBuf,
    response_active: bool,
    /// A query file waiting for detection before it can run (`true`) or
    /// pick its connection (`false`).
    pending_query: Option<(PathBuf, bool)>,
    file_diff: Option<Entity<FileDiff>>,
    /// A table's structure, shown in place of the editors.
    structure: Option<(Entity<crate::structure::StructureView>, Subscription)>,
    /// A connection's ERD, shown in place of the editors (under a structure
    /// form opened from it).
    erd: Option<(Entity<crate::erd_view::ErdView>, Subscription)>,
    diff_task: Option<Task<()>>,
    diff_subscription: Option<Subscription>,
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
        let services = cx.new(|cx| ServicesPanel::new(root.clone(), cx));
        let database = cx.new(|_| DatabaseStore::new(root.clone()));
        let database_panel = cx.new(|cx| DatabasePanel::new(database.clone(), cx));
        let api_panel = cx.new(|cx| crate::api_panel::ApiPanel::new(root.clone(), cx));
        let ai_store = crate::ai_store::AiStore::global(cx);
        let ai_panel = cx.new(|cx| crate::ai_panel::AiPanel::new(ai_store.clone(), cx));
        let weak = cx.entity().downgrade();
        let chat = cx.new(|cx| crate::chat_panel::ChatPanel::new(ai_store, weak, cx));
        let results = cx.new(|cx| ResultsView::new(database.clone(), cx));
        let response = cx.new(crate::response::ResponseView::new);
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
                    if paths.iter().any(|p| is_service_manifest(p)) {
                        this.services.update(cx, |s, cx| s.redetect(cx));
                    }
                    if paths.iter().any(|p| database::is_connection_source(p))
                        && this.database.read(cx).detected()
                    {
                        this.database.update(cx, |d, cx| d.redetect(cx));
                    }
                }
                ProjectEvent::GitChanged => this.git.update(cx, |g, cx| g.refresh(cx)),
                ProjectEvent::Scanned => cx.notify(),
            }),
            cx.subscribe_in(
                &services,
                window,
                |this, _, event, window, cx| match event {
                    ServicesEvent::Start(spec) => this.start_service(spec.clone(), window, cx),
                    ServicesEvent::Reveal(terminal) => this.reveal_terminal(terminal, window, cx),
                    ServicesEvent::Kill(terminal) => this.remove_terminal(terminal, window, cx),
                    ServicesEvent::ContainerLogs(container) => {
                        let command = TerminalCommand {
                            program: services::docker_path().map(|p| p.display().to_string()),
                            args: vec![
                                "logs".into(),
                                "-f".into(),
                                "--tail".into(),
                                "200".into(),
                                container.id.clone(),
                            ],
                            cwd: this.root(cx),
                            title: Some(format!("logs {}", container.name)),
                            keep_on_exit: true,
                            ..Default::default()
                        };
                        this.spawn_terminal(command, window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &database_panel,
                window,
                |this, _, event, window, cx| match event {
                    DatabasePanelEvent::Run { connection, query } => {
                        this.run_query(connection.clone(), query.clone(), false, window, cx)
                    }
                    DatabasePanelEvent::Browse {
                        connection,
                        engine,
                        spec,
                    } => {
                        let (connection, engine, spec) =
                            (connection.clone(), *engine, spec.clone());
                        this.results
                            .update(cx, |r, cx| r.browse(connection, engine, spec, cx));
                        this.show_results = true;
                        this.results_active = true;
                        this.response_active = false;
                        this.dock_open = true;
                        cx.notify();
                    }
                    DatabasePanelEvent::Structure {
                        connection,
                        engine,
                        object,
                    } => {
                        this.open_structure(connection.clone(), *engine, object.clone(), window, cx)
                    }
                    DatabasePanelEvent::Diagram { connection, engine } => {
                        this.open_erd(connection.clone(), *engine, window, cx)
                    }
                    DatabasePanelEvent::NewKey { connection } => {
                        let workspace = cx.weak_entity();
                        let prompt = crate::key_prompts::NewKeyPrompt::new(
                            this.results.downgrade(),
                            connection.clone(),
                            move |_, cx| {
                                workspace
                                    .update(cx, |ws, cx| {
                                        ws.show_results = true;
                                        ws.results_active = true;
                                        ws.response_active = false;
                                        ws.dock_open = true;
                                        cx.notify();
                                    })
                                    .ok();
                            },
                        );
                        this.toggle_modal(window, cx, move |window, cx| {
                            Picker::new(prompt, window, cx)
                        });
                    }
                    DatabasePanelEvent::NewQuery { connection } => {
                        this.open_scratch_query(connection.to_string(), window, cx)
                    }
                },
            ),
            cx.subscribe_in(
                &results,
                window,
                |this, results, event, window, cx| match event {
                    crate::results::ResultsEvent::KeyPrompt { action, key, .. } => {
                        let prompt = crate::key_prompts::KeyPrompt::new(
                            results.downgrade(),
                            *action,
                            key.clone(),
                        );
                        this.toggle_modal(window, cx, move |window, cx| {
                            Picker::new(prompt, window, cx)
                        });
                    }
                    crate::results::ResultsEvent::PickReference {
                        row,
                        column,
                        connection,
                        query,
                        title,
                    } => {
                        let picker = crate::reference_picker::ReferencePicker::new(
                            results.downgrade(),
                            this.database.clone(),
                            *row,
                            *column,
                            connection.clone(),
                            query.clone(),
                            title.clone(),
                        );
                        this.toggle_modal(window, cx, move |window, cx| {
                            Picker::new(picker, window, cx)
                        });
                    }
                },
            ),
            cx.subscribe_in(
                &api_panel,
                window,
                |this, _, event, window, cx| match event {
                    crate::api_panel::ApiEvent::Open(request) => {
                        this.add_request(request.clone(), window, cx)
                    }
                    crate::api_panel::ApiEvent::Send(request) => {
                        this.send_route_request(request.clone(), cx)
                    }
                },
            ),
            cx.subscribe_in(
                &database,
                window,
                |this, _, _: &DatabaseEvent, window, cx| {
                    this.attach_query_editors(cx);
                    if this.database.read(cx).detected()
                        && let Some((path, run)) = this.pending_query.take()
                    {
                        this.query_file_action(path, run, window, cx);
                    }
                    cx.notify();
                },
            ),
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
                    GitPanelEvent::ReviewDiff(path, scope) => {
                        this.open_file_diff(path.clone(), *scope, window, cx)
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
            services,
            database,
            database_panel,
            api_panel: api_panel.clone(),
            ai_panel,
            chat,
            chat_open: false,
            results,
            show_results: false,
            results_active: false,
            response,
            show_response: false,
            scratch_dir: settings::config_dir().join("scratch"),
            response_active: false,
            pending_query: None,
            structure: None,
            erd: None,
            file_diff: None,
            diff_task: None,
            diff_subscription: None,
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
                EditorEvent::Edited | EditorEvent::Saved => {
                    if this.file_diff.as_ref().is_some_and(|view| {
                        view.read(cx).scope == DiffScope::Working
                            && editor.read(cx).path(cx) == Some(view.read(cx).path.as_path())
                    }) {
                        this.load_file_diff(cx);
                    }
                }
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
        self.attach_query_editor(&editor, cx);
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
        self.close_file_diff(window, cx);
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
        self.close_file_diff(window, cx);
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
        if self.file_diff.is_some() {
            self.close_file_diff(window, cx);
            return;
        }
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
        self.load_file_diff(cx);
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

    fn open_file_diff(
        &mut self,
        path: PathBuf,
        scope: DiffScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| FileDiff::new(path, scope, cx));
        self.diff_subscription =
            Some(
                cx.subscribe_in(&view, window, |this, view, event, window, cx| match event {
                    FileDiffEvent::Close => this.close_file_diff(window, cx),
                    FileDiffEvent::Refresh => this.load_file_diff(cx),
                    FileDiffEvent::OpenFile => {
                        let path = view.read(cx).path.clone();
                        this.open_path(path, None, window, cx);
                    }
                }),
            );
        window.focus(&view.focus_handle(cx));
        self.file_diff = Some(view);
        self.load_file_diff(cx);
        cx.notify();
    }

    fn load_file_diff(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.file_diff.clone() else {
            return;
        };
        let Some(repo) = self.git.read(cx).repo().cloned() else {
            view.update(cx, |view, cx| {
                view.set_result(
                    Err(crate::git::GitError(
                        "Open a file inside a Git repository.".into(),
                    )),
                    cx,
                )
            });
            return;
        };
        let path = view.read(cx).path.clone();
        let scope = view.read(cx).scope;
        let Some(rel) = repo.relative(&path) else {
            view.update(cx, |view, cx| {
                view.set_result(
                    Err(crate::git::GitError(
                        "This file is outside the Git repository.".into(),
                    )),
                    cx,
                )
            });
            return;
        };
        let working = if scope == DiffScope::Working {
            self.document_for_path(&path, cx)
                .filter(|document| document.read(cx).is_dirty())
                .map(|document| {
                    let text = document.read(cx).text();
                    (text.rope().clone(), text.line_ending().as_str())
                })
        } else {
            None
        };
        // Reuse the status the workspace keeps fresh; a status change reloads
        // this comparison anyway.
        let status = self.git.read(cx).loaded_status();
        self.diff_task = Some(cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    repo.file_diff(
                        &rel,
                        scope,
                        working.map(|(rope, ending)| match ending {
                            "\n" => rope.to_string(),
                            ending => rope.to_string().replace('\n', ending),
                        }),
                        status.as_deref(),
                    )
                    .map(DiffModel::new)
                })
                .await;
            view.update(cx, |view, cx| view.set_result(result, cx)).ok();
        }));
    }

    fn close_file_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_diff.take().is_none() {
            return;
        }
        self.diff_task = None;
        self.diff_subscription = None;
        if let Some(editor) = self.active_editor() {
            window.focus(&editor.focus_handle(cx));
        } else {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    fn show_file_diff(&mut self, _: &ShowFileDiff, window: &mut Window, cx: &mut Context<Self>) {
        let path = self
            .active_editor()
            .and_then(|editor| editor.read(cx).path(cx).map(Path::to_path_buf));
        if let Some(path) = path {
            self.open_file_diff(path, DiffScope::Working, window, cx);
        }
    }

    fn show_git(&mut self, _: &ShowGit, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(SidebarTab::Git), cx);
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
        self.spawn_terminal_with(command, true, window, cx)
    }

    /// Like `spawn_terminal`; without `focus` the dock opens on the new tab
    /// but keyboard focus stays where it was (starting a whole stack from the
    /// services tab).
    fn spawn_terminal_with(
        &mut self,
        command: TerminalCommand,
        focus: bool,
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
                // Services keep their tab so a crash's output stays readable.
                TerminalEvent::Exited if terminal.read(cx).keep_on_exit => cx.notify(),
                TerminalEvent::Exited => this.remove_terminal(terminal, window, cx),
            },
        );
        self.terminals.push((terminal.clone(), subscription));
        self.active_terminal = self.terminals.len() - 1;
        self.results_active = false;
        self.response_active = false;
        self.dock_open = true;
        if focus {
            window.focus(&terminal.focus_handle(cx));
        }
        cx.notify();
        Some(terminal)
    }

    /// Runs a detected service in a dock terminal through the login shell.
    fn start_service(&mut self, spec: ServiceSpec, window: &mut Window, cx: &mut Context<Self>) {
        let (shell, mut args) = services::login_shell();
        args.push(spec.command.clone());
        let mut env: std::collections::HashMap<String, String> =
            spec.env.clone().into_iter().collect();
        // Most dev servers only color their output when asked.
        env.entry("FORCE_COLOR".into())
            .or_insert_with(|| "1".into());
        let command = TerminalCommand {
            program: Some(shell),
            args,
            cwd: self.root(cx).join(&spec.dir),
            env,
            title: Some(spec.name.clone()),
            keep_on_exit: true,
        };
        if let Some(terminal) = self.spawn_terminal_with(command, false, window, cx) {
            self.services
                .update(cx, |s, cx| s.attach(&spec.name, &terminal, cx));
        }
    }

    fn reveal_terminal(
        &mut self,
        terminal: &Entity<Terminal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.terminals.iter().position(|(t, _)| t == terminal) {
            self.active_terminal = ix;
            self.results_active = false;
            self.response_active = false;
            self.dock_open = true;
            window.focus(&terminal.focus_handle(cx));
            cx.notify();
        }
    }

    fn show_services(&mut self, _: &ShowServices, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(SidebarTab::Services), cx);
        window.focus(&self.services.focus_handle(cx));
    }

    /// Changes the sidebar tab and tells panels that poll whether they are
    /// on screen.
    fn set_sidebar(&mut self, tab: Option<SidebarTab>, cx: &mut Context<Self>) {
        self.sidebar = tab;
        let visible = tab == Some(SidebarTab::Services);
        self.services.update(cx, |s, cx| s.set_visible(visible, cx));
        if tab == Some(SidebarTab::Database) {
            self.database_panel.update(cx, |p, cx| p.shown(cx));
        }
        if tab == Some(SidebarTab::Api) {
            self.api_panel.update(cx, |p, cx| p.shown(cx));
        }
        if tab == Some(SidebarTab::Ai) {
            self.ai_panel.update(cx, |p, cx| p.shown(cx));
        }
        cx.notify();
    }

    fn show_database(&mut self, _: &ShowDatabase, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(SidebarTab::Database), cx);
        window.focus(&self.database_panel.focus_handle(cx));
    }

    fn new_connection(&mut self, _: &NewConnection, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = NewConnectionPrompt::new(self.database.clone());
        self.toggle_modal(window, cx, move |window, cx| {
            Picker::new(prompt, window, cx)
        });
    }

    /// Runs `query` and shows the Results tab. `focus` moves the keyboard
    /// there; running from an editor keeps it in the editor.
    pub fn run_query(
        &mut self,
        connection: SharedString,
        query: String,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.results
            .update(cx, |r, cx| r.run(connection, query, cx));
        self.show_results = true;
        self.results_active = true;
        self.response_active = false;
        self.dock_open = true;
        if focus {
            window.focus(&self.results.focus_handle(cx));
        }
        cx.notify();
    }

    // ------------------------------------------------------------ query files

    /// Gives a query file its connection's schema for completion, choosing
    /// the default connection for a file that has none.
    fn attach_query_editor(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let Some(path) = editor.read(cx).path(cx).map(Path::to_path_buf) else {
            return;
        };
        let store = self.database.clone();
        let bound = store.read(cx).binding(&path).map(str::to_string);
        let name = match bound {
            Some(name) => {
                store.update(cx, |s, cx| s.ensure_schema(&name, cx));
                name
            }
            None => {
                let Some(engines) = database::query_file_engines(&path) else {
                    return;
                };
                if !store.read(cx).detected() {
                    // Attached again when detection finishes.
                    store.update(cx, |s, cx| s.ensure_detected(cx));
                    return;
                }
                let Some(name) = store.read(cx).default_for(engines) else {
                    return;
                };
                store.update(cx, |s, cx| s.bind(path, name.clone(), cx));
                name
            }
        };
        let source: editor::SchemaSource =
            std::rc::Rc::new(move |cx: &App| store.read(cx).schema_of(&name));
        editor.update(cx, |e, _| e.set_schema_source(Some(source)));
    }

    fn attach_query_editors(&mut self, cx: &mut Context<Self>) {
        let editors: Vec<_> = self.all_editors().cloned().collect();
        for editor in editors {
            self.attach_query_editor(&editor, cx);
        }
    }

    fn run_statement(&mut self, _: &RunStatement, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor().cloned()
            && editor
                .read(cx)
                .path(cx)
                .and_then(|p| p.extension())
                .is_some_and(|e| e == "http" || e == "rest")
        {
            self.send_request_at_cursor(&editor, cx);
            return;
        }
        let path = self
            .active_editor()
            .and_then(|e| e.read(cx).path(cx).map(Path::to_path_buf));
        let Some(path) = path.filter(|p| {
            database::query_file_engines(p).is_some() || self.database.read(cx).binding(p).is_some()
        }) else {
            cx.propagate();
            return;
        };
        self.query_file_action(path, true, window, cx);
    }

    fn select_connection(
        &mut self,
        _: &SelectConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .active_editor()
            .and_then(|e| e.read(cx).path(cx).map(Path::to_path_buf));
        if let Some(path) = path {
            self.query_file_action(path, false, window, cx);
        }
    }

    /// Runs the statement under the cursor (`run`) or picks a connection for
    /// `path`, once connections are known.
    fn query_file_action(
        &mut self,
        path: PathBuf,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.database.read(cx);
        if !store.detected() {
            self.pending_query = Some((path, run));
            self.database.update(cx, |s, cx| s.ensure_detected(cx));
            return;
        }
        let name = store.binding(&path).map(str::to_string).or_else(|| {
            database::query_file_engines(&path).and_then(|engines| store.default_for(engines))
        });
        match name {
            Some(name) if run => self.connection_chosen(path, name, true, window, cx),
            _ => {
                let picker =
                    ConnectionPicker::new(cx.weak_entity(), self.database.read(cx), path, run);
                self.toggle_modal(window, cx, move |window, cx| {
                    Picker::new(picker, window, cx)
                });
            }
        }
    }

    /// Binds `path` to the connection and, with `run`, runs the statement
    /// under the cursor of the active editor on it.
    pub fn connection_chosen(
        &mut self,
        path: PathBuf,
        name: String,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.database
            .update(cx, |s, cx| s.bind(path.clone(), name.clone(), cx));
        self.attach_query_editors(cx);
        let Some(editor) = self.active_editor().cloned() else {
            return;
        };
        if !run || editor.read(cx).path(cx) != Some(path.as_path()) {
            return;
        }
        let Some(engine) = self.database.read(cx).engine_of(&name) else {
            return;
        };
        let e = editor.read(cx);
        let range = e.newest_range();
        let buffer = e.document().read(cx).text();
        let text = buffer.text_for_range(0..buffer.len());
        let query = if range.is_empty() {
            db::sql::statement_at(engine, &text, range.end).map(|r| text[r].to_string())
        } else {
            Some(text[range].to_string())
        };
        if let Some(query) = query.filter(|q| !q.trim().is_empty()) {
            self.run_query(name.into(), query, false, window, cx);
        }
    }

    /// Opens the connection's scratch file, kept in the config directory so
    /// it survives restarts without cluttering the project.
    fn open_scratch_query(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(engine) = self.database.read(cx).engine_of(&name) else {
            return;
        };
        let extension = match engine {
            db::Engine::Redis => "redis",
            db::Engine::Mongo => "mongodb",
            _ => "sql",
        };
        let path = self.scratch_file(&name, extension, cx);
        let create = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let created = cx
                .background_executor()
                .spawn(async move {
                    std::fs::create_dir_all(create.parent().unwrap_or(Path::new(".")))?;
                    if !create.exists() {
                        std::fs::write(&create, "")?;
                    }
                    std::io::Result::Ok(())
                })
                .await;
            if let Err(e) = created {
                eprintln!("could not create the scratch file: {e}");
                return;
            }
            this.update_in(cx, |this, window, cx| {
                this.database
                    .update(cx, |s, cx| s.bind(path.clone(), name, cx));
                this.open_path(path, None, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Shows a table's structure (or a new table) in place of the editors.
    pub fn open_structure(
        &mut self,
        connection: SharedString,
        engine: db::Engine,
        object: Option<db::Object>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::structure::{StructureEvent, StructureView};
        let (store, root) = (self.database.clone(), self.root(cx));
        let table = object.as_ref().map(db::ddl::TableDraft::of);
        let view = cx.new(|cx| StructureView::new(store, root, connection, engine, table, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
                StructureEvent::Close => this.close_structure(window, cx),
                StructureEvent::OpenFile(path) => this.open_path(path.clone(), None, window, cx),
            });
        window.focus(&view.focus_handle(cx));
        self.structure = Some((view, subscription));
        cx.notify();
    }

    /// Shows a connection's ERD in place of the editors.
    pub fn open_erd(
        &mut self,
        connection: SharedString,
        engine: db::Engine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::erd_view::{ErdEvent, ErdView};
        let (store, root) = (self.database.clone(), self.root(cx));
        let view = cx.new(|cx| ErdView::new(store, root, connection.clone(), engine, cx));
        let subscription =
            cx.subscribe_in(
                &view,
                window,
                move |this, _, event, window, cx| match event {
                    ErdEvent::Close => {
                        this.erd = None;
                        this.close_structure(window, cx);
                        cx.notify();
                    }
                    ErdEvent::Browse(object) => {
                        let spec = db::browse::Browse::of(object);
                        let connection = connection.clone();
                        this.results
                            .update(cx, |r, cx| r.browse(connection, engine, spec, cx));
                        this.show_results = true;
                        this.results_active = true;
                        this.response_active = false;
                        this.dock_open = true;
                        cx.notify();
                    }
                    ErdEvent::Structure(object) => {
                        this.open_structure(connection.clone(), engine, object.clone(), window, cx)
                    }
                    ErdEvent::Link(object, fk) => {
                        this.open_structure(
                            connection.clone(),
                            engine,
                            Some(object.clone()),
                            window,
                            cx,
                        );
                        if let Some((view, _)) = &this.structure {
                            view.update(cx, |v, cx| v.add_foreign_key_draft(fk, cx));
                        }
                    }
                    ErdEvent::RunQuery(query) => {
                        this.run_query(connection.clone(), query.clone(), false, window, cx)
                    }
                },
            );
        window.focus(&view.focus_handle(cx));
        self.erd = Some((view, subscription));
        cx.notify();
    }

    fn close_structure(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.structure.take().is_some() {
            // Back to the ERD it was opened from, if any.
            if let Some((erd, _)) = &self.erd {
                window.focus(&erd.focus_handle(cx));
                cx.notify();
                return;
            }
            match self.active_editor() {
                Some(editor) => window.focus(&editor.focus_handle(cx)),
                None => window.focus(&self.focus_handle),
            }
            cx.notify();
        }
    }

    // ------------------------------------------------------------ HTTP

    fn toggle_chat(&mut self, _: &ToggleChat, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.chat.read(cx).input();
        if self.chat_open && !input.focus_handle(cx).is_focused(window) {
            // Open but elsewhere: go to it rather than close it.
            window.focus(&input.focus_handle(cx));
            return;
        }
        self.chat_open = !self.chat_open;
        if self.chat_open {
            self.chat.update(cx, |c, cx| c.shown(cx));
            window.focus(&input.focus_handle(cx));
        } else if let Some(editor) = self.active_editor() {
            window.focus(&editor.focus_handle(cx));
        }
        cx.notify();
    }

    /// The active file for the chat: its selection when there is one, else
    /// the whole file, with its path from the project root.
    pub fn file_context(&self, cx: &App) -> Option<crate::chat_panel::FileContext> {
        let editor = self.active_editor()?.read(cx);
        let path = editor.path(cx)?;
        if !ai::context::allows_file(path) {
            return None;
        }
        let root = self.root(cx);
        let path = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        let range = editor.newest_range();
        let text = editor.text(cx);
        if range.is_empty() || range.end > text.len() {
            return Some(crate::chat_panel::FileContext {
                path,
                text,
                part: None,
            });
        }
        let buffer = editor.buf(cx);
        let first = buffer.offset_to_point(range.start).row + 1;
        let last = buffer.offset_to_point(range.end).row + 1;
        Some(crate::chat_panel::FileContext {
            path,
            text: text[range].to_string(),
            part: Some(if first == last {
                format!("line {first}")
            } else {
                format!("lines {first}-{last}")
            }),
        })
    }

    fn show_ai(&mut self, _: &ShowAi, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(SidebarTab::Ai), cx);
        window.focus(&self.ai_panel.focus_handle(cx));
    }

    fn show_api(&mut self, _: &ShowApi, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(SidebarTab::Api), cx);
        window.focus(&self.api_panel.focus_handle(cx));
    }

    /// Where the project's server listens: a running service's port, else
    /// `PORT` in `.env`, else the usual port of its framework.
    fn base_url(&self, cx: &App) -> String {
        let running = self.services.read(cx).running_ports(cx);
        let env_port = std::fs::read_to_string(self.root(cx).join(".env"))
            .ok()
            .and_then(|t| db::parse_env(&t).get("PORT")?.parse().ok());
        let framework = self.api_panel.read(cx).framework();
        let port = running
            .first()
            .copied()
            .unwrap_or_else(|| rest::routes::default_port(framework, env_port));
        format!("http://localhost:{port}")
    }

    /// The project's requests file, kept with the scratch queries.
    pub fn requests_path(&self, cx: &App) -> PathBuf {
        self.scratch_file("requests", "http", cx)
    }

    /// `<project>-<name>.<extension>` in the scratch directory, outside the project so
    /// it survives restarts without cluttering it.
    fn scratch_file(&self, name: &str, extension: &str, cx: &App) -> PathBuf {
        let project = self
            .root(cx)
            .file_name()
            .map_or("project".into(), |n| n.to_string_lossy().into_owned());
        let safe = |s: &str| -> String {
            s.chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect()
        };
        self.scratch_dir
            .join(format!("{}-{}.{extension}", safe(&project), safe(name)))
    }

    fn show_request_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.response.update(cx, |r, cx| {
            r.request = None;
            r.state = crate::response::ResponseState::Failed(error.into());
            cx.notify();
        });
        self.show_response = true;
        self.response_active = true;
        self.results_active = false;
        self.dock_open = true;
        cx.notify();
    }

    fn requests_header(&self, cx: &App) -> String {
        format!(
            "# Requests for this project. cmd-enter sends the one under the cursor.\n@baseUrl = {}\n",
            self.base_url(cx)
        )
    }

    fn open_requests(
        &mut self,
        _: &crate::api_panel::OpenRequests,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.requests_path(cx);
        let header = self.requests_header(cx);
        let create = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let made = cx
                .background_executor()
                .spawn(async move {
                    std::fs::create_dir_all(create.parent().unwrap_or(Path::new(".")))?;
                    if !create.exists() {
                        std::fs::write(&create, header)?;
                    }
                    std::io::Result::Ok(())
                })
                .await;
            if made.is_ok() {
                this.update_in(cx, |this, window, cx| {
                    this.open_path(path, None, window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Adds `request` to the end of the requests file and opens it there,
    /// the cursor on its request line.
    pub fn add_request(&mut self, request: String, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.requests_path(cx);
        // An open, possibly unsaved, requests file gets the text in place.
        if let Some(document) = self.document_for_path(&path, cx) {
            let end = document.read(cx).text().len();
            let row = document.read(cx).text().offset_to_point(end).row;
            let text = format!("\n{request}");
            document.update(cx, |d, cx| {
                d.apply_edits(vec![(end..end, text)], None, cx);
            });
            self.open_path(
                path,
                Some(Jump::Point {
                    row: row + 2,
                    column: 0,
                }),
                window,
                cx,
            );
            return;
        }
        let header = self.requests_header(cx);
        let write = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let row = cx
                .background_executor()
                .spawn(async move {
                    std::fs::create_dir_all(write.parent().unwrap_or(Path::new(".")))?;
                    let mut text = std::fs::read_to_string(&write).unwrap_or(header);
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                    let row = text.lines().count() + 2;
                    text.push('\n');
                    text.push_str(&request);
                    std::fs::write(&write, text)?;
                    std::io::Result::Ok(row)
                })
                .await;
            if let Ok(row) = row {
                this.update_in(cx, |this, window, cx| {
                    this.open_path(path, Some(Jump::Point { row, column: 0 }), window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Sends a route's request right away; `{{baseUrl}}` is where the
    /// project's server listens.
    fn send_route_request(&mut self, request: String, cx: &mut Context<Self>) {
        let mut env = std::fs::read_to_string(self.root(cx).join(".env"))
            .map(|t| db::parse_env(&t))
            .unwrap_or_default();
        env.entry("baseUrl".into())
            .or_insert_with(|| self.base_url(cx));
        let Some(block) = rest::request_at(&request, 0) else {
            return;
        };
        match rest::http_file::parse(&request, &block, &env) {
            Ok(request) => self.send_request(request, cx),
            Err(e) => self.show_request_error(e, cx),
        }
    }

    /// Writes an OpenAPI file's operations to a `.http` file next to it and
    /// opens that.
    fn import_openapi(
        &mut self,
        _: &crate::api_panel::ImportOpenApi,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = answer.await else {
                return;
            };
            let Some(spec) = paths.into_iter().next() else {
                return;
            };
            let import = cx
                .background_executor()
                .spawn(async move { import_openapi_file(&spec) })
                .await;
            this.update_in(cx, |this, window, cx| match import {
                Ok(path) => {
                    this.api_panel.update(cx, |p, cx| p.reload(cx));
                    this.open_path(path, None, window, cx);
                }
                Err(e) => this.show_request_error(e, cx),
            })
            .ok();
        })
        .detach();
    }

    fn close_response(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was_focused = self.response.focus_handle(cx).contains_focused(window, cx);
        self.show_response = false;
        self.response_active = false;
        self.results_active = self.show_results;
        if self.terminals.is_empty() && !self.show_results {
            self.dock_open = false;
        }
        if was_focused {
            match self.active_editor() {
                Some(e) => window.focus(&e.focus_handle(cx)),
                None => window.focus(&self.focus_handle),
            }
        }
        cx.notify();
    }

    /// Sends `request` and shows the Response tab; the keyboard stays where
    /// it was.
    pub fn send_request(&mut self, request: rest::Request, cx: &mut Context<Self>) {
        self.response.update(cx, |r, cx| r.send(request, cx));
        self.show_response = true;
        self.response_active = true;
        self.results_active = false;
        self.dock_open = true;
        cx.notify();
    }

    /// `cmd-enter` in a `.http` file: the request under the cursor, with
    /// `{{variables}}` from the file and then the project's `.env`.
    fn send_request_at_cursor(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let e = editor.read(cx);
        let offset = e.newest_range().end;
        let text = e.text(cx);
        let Some(block) = rest::request_at(&text, offset) else {
            return;
        };
        let env = std::fs::read_to_string(self.root(cx).join(".env"))
            .map(|t| db::parse_env(&t))
            .unwrap_or_default();
        match rest::http_file::parse(&text, &block, &env) {
            Ok(request) => self.send_request(request, cx),
            Err(e) => self.show_request_error(e, cx),
        }
    }

    fn close_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was_focused = self.results.focus_handle(cx).contains_focused(window, cx);
        self.show_results = false;
        self.results_active = false;
        self.response_active = self.show_response;
        if self.terminals.is_empty() && !self.show_response {
            self.dock_open = false;
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
            self.dock_open = self.dock_open && (self.show_results || self.show_response);
            self.results_active = self.show_results;
            self.response_active = !self.show_results && self.show_response;
            self.active_terminal = 0;
        } else {
            self.active_terminal = self.active_terminal.min(self.terminals.len() - 1);
        }
        if was_focused {
            match (
                self.terminals.get(self.active_terminal),
                self.active_editor(),
            ) {
                _ if self.results_active => window.focus(&self.results.focus_handle(cx)),
                _ if self.response_active => window.focus(&self.response.focus_handle(cx)),
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
            Some(t)
                if self.dock_open
                    && !self.results_active
                    && !self.response_active
                    && t.focus_handle(cx).contains_focused(window, cx) =>
            {
                self.dock_open = false;
                if let Some(e) = self.active_editor() {
                    window.focus(&e.focus_handle(cx));
                }
                cx.notify();
            }
            Some(t) => {
                self.dock_open = true;
                self.results_active = false;
                self.response_active = false;
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
                let active =
                    ix == self.active_terminal && !self.results_active && !self.response_active;
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
                        this.results_active = false;
                        this.response_active = false;
                        if let Some((t, _)) = this.terminals.get(ix) {
                            window.focus(&t.focus_handle(cx));
                        }
                        cx.notify();
                    }))
                    .child({
                        let t = terminal.read(cx);
                        match (t.exited, t.exit_code) {
                            (false, _) => t.title().to_string(),
                            (true, Some(0)) => format!("{} (exited)", t.title()),
                            (true, Some(code)) => format!("{} (exited {code})", t.title()),
                            (true, None) => format!("{} (killed)", t.title()),
                        }
                    })
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
                    .when(self.show_response, |d| {
                        d.child(self.render_response_tab(&theme, cx))
                    })
                    .when(self.show_results, |d| {
                        d.child(self.render_results_tab(&theme, cx))
                    })
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
            .child(div().flex_1().min_h_0().map(|d| {
                if self.response_active {
                    d.child(self.response.clone())
                } else if self.results_active {
                    d.child(self.results.clone())
                } else {
                    d.children(
                        self.terminals
                            .get(self.active_terminal)
                            .map(|(t, _)| t.clone()),
                    )
                }
            }))
    }

    fn render_results_tab(
        &self,
        theme: &crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.results_active;
        div()
            .id("results-tab")
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
            .on_click(cx.listener(|this, _, window, cx| {
                this.results_active = true;
                this.response_active = false;
                window.focus(&this.results.focus_handle(cx));
                cx.notify();
            }))
            .child("Results")
            .child(
                div()
                    .id("results-close")
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .hover(|d| d.bg(theme.line))
                    .child("×")
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_results(window, cx);
                    })),
            )
    }

    fn render_response_tab(
        &self,
        theme: &crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.response_active;
        div()
            .id("response-tab")
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
            .on_click(cx.listener(|this, _, window, cx| {
                this.response_active = true;
                this.results_active = false;
                window.focus(&this.response.focus_handle(cx));
                cx.notify();
            }))
            .child("Response")
            .child(
                div()
                    .id("response-close")
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .hover(|d| d.bg(theme.line))
                    .child("×")
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_response(window, cx);
                    })),
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
        self.set_sidebar(Some(SidebarTab::Files), cx);
        window.focus(&self.project_panel.focus_handle(cx));
        cx.notify();
    }

    fn show_search(&mut self, _: &ShowSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(SidebarTab::Search), cx);
        let selected = self
            .active_editor()
            .and_then(|e| e.read(cx).selected_text(cx));
        self.project_search
            .update(cx, |s, cx| s.focus_query(selected, window, cx));
        cx.notify();
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        let tab = match self.sidebar {
            Some(_) => None,
            None => Some(SidebarTab::Files),
        };
        self.set_sidebar(tab, cx);
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
        self.set_sidebar(Some(SidebarTab::Files), cx);
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
                .px_1()
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
                    SidebarTab::Services => this.show_services(&ShowServices, window, cx),
                    SidebarTab::Database => this.show_database(&ShowDatabase, window, cx),
                    SidebarTab::Api => this.show_api(&ShowApi, window, cx),
                    SidebarTab::Ai => this.show_ai(&ShowAi, window, cx),
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
                    .child(tab_button("sidebar-git", "Git", SidebarTab::Git))
                    .child(tab_button(
                        "sidebar-services",
                        "Services",
                        SidebarTab::Services,
                    ))
                    .child(tab_button(
                        "sidebar-database",
                        "Database",
                        SidebarTab::Database,
                    ))
                    .child(tab_button("sidebar-api", "API", SidebarTab::Api))
                    .child(tab_button("sidebar-ai", "AI", SidebarTab::Ai)),
            )
            .child(div().flex_1().min_h_0().pt_1().map(|d| match tab {
                SidebarTab::Files => d.child(self.project_panel.clone()),
                SidebarTab::Search => d.child(self.project_search.clone()),
                SidebarTab::Git => d.child(self.git_panel.clone()),
                SidebarTab::Services => d.child(self.services.clone()),
                SidebarTab::Database => d.child(self.database_panel.clone()),
                SidebarTab::Api => d.child(self.api_panel.clone()),
                SidebarTab::Ai => d.child(self.ai_panel.clone()),
            }))
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut left: Vec<String> = Vec::new();
        if self.file_diff.is_some() {
            left.push("Git diff".into());
            left.push("Read-only".into());
        } else if let Some(editor) = self.active_editor() {
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

        let connection = self.active_editor().and_then(|e| {
            let path = e.read(cx).path(cx)?;
            let store = self.database.read(cx);
            match store.binding(path) {
                Some(name) => Some(name.to_string()),
                None => database::query_file_engines(path).map(|_| "No connection".to_string()),
            }
        });
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
                    .children(connection.map(|name| {
                        div()
                            .id("status-connection")
                            .px_1p5()
                            .rounded(px(6.))
                            .text_color(theme.fg_muted)
                            .hover(|d| d.bg(theme.line).text_color(theme.fg))
                            .child(name)
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(SelectConnection), cx)
                            })
                    }))
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

/// Files whose change can add, remove or alter a detected service.
/// `api.yaml` becomes `api.http` beside it.
pub fn import_openapi_file(spec: &Path) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(spec).map_err(|e| format!("{}: {e}", spec.display()))?;
    let parsed = rest::openapi::parse(&text)?;
    let out = spec.with_extension("http");
    std::fs::write(&out, rest::openapi::to_http(&parsed)).map_err(|e| e.to_string())?;
    Ok(out)
}

fn is_service_manifest(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    matches!(
        name,
        "package.json"
            | "go.mod"
            | "main.go"
            | "Cargo.toml"
            | "manage.py"
            | "main.py"
            | "app.py"
            | "services.json"
            | "compose.yaml"
            | "compose.yml"
            | "docker-compose.yaml"
            | "docker-compose.yml"
    )
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
            .structure
            .as_ref()
            .map(|(view, _)| format!("Structure of {}", view.read(cx).title(cx)))
            .or_else(|| {
                self.erd
                    .as_ref()
                    .map(|(view, _)| format!("Diagram of {}", view.read(cx).connection))
            })
            .or_else(|| {
                self.file_diff
                    .as_ref()
                    .map(|view| view.read(cx).path.display().to_string())
            })
            .or_else(|| {
                self.active_editor().and_then(|editor| {
                    editor
                        .read(cx)
                        .path(cx)
                        .map(|path| path.display().to_string())
                })
            })
            .unwrap_or_else(|| root.display().to_string());
        window.set_window_title(&title);
        let special: Option<AnyView> = self
            .structure
            .as_ref()
            .map(|(view, _)| view.clone().into())
            .or_else(|| self.erd.as_ref().map(|(view, _)| view.clone().into()))
            .or_else(|| self.file_diff.as_ref().map(|view| view.clone().into()));
        let panes: Vec<_> = match special {
            Some(view) => vec![
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(view.clone())
                    .into_any_element(),
            ],
            None => (0..self.panes.len())
                .map(|p| self.render_pane(p, window, cx).into_any_element())
                .collect(),
        };

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
            .on_action(cx.listener(Self::show_services))
            .on_action(cx.listener(Self::show_database))
            .on_action(cx.listener(Self::show_api))
            .on_action(cx.listener(Self::show_ai))
            .on_action(cx.listener(Self::toggle_chat))
            .on_action(cx.listener(Self::open_requests))
            .on_action(cx.listener(Self::import_openapi))
            .on_action(cx.listener(Self::new_connection))
            .on_action(cx.listener(Self::run_statement))
            .on_action(cx.listener(Self::select_connection))
            // Available from anywhere (command palette), not just the tab.
            .on_action(cx.listener(|this, _: &RunStack, _, cx| {
                this.services.update(cx, |s, cx| s.start_all(cx))
            }))
            .on_action(cx.listener(|this, _: &StopAll, _, cx| {
                this.services.update(cx, |s, cx| s.stop_all_services(cx))
            }))
            .on_action(cx.listener(Self::show_file_diff))
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
                    .children(panes)
                    .when(self.chat_open, |d| {
                        d.child(
                            div()
                                .w(px(CHAT_WIDTH))
                                .flex_none()
                                .h_full()
                                .border_l_1()
                                .border_color(theme.line)
                                .child(self.chat.clone()),
                        )
                    }),
            )
            .when(
                self.dock_open
                    && (!self.terminals.is_empty() || self.show_results || self.show_response),
                |d| d.child(self.render_dock(cx)),
            )
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
        let dir = db::testing::dir(&format!("ws-{name}"));
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
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
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
    fn search_navigation_keys_preserve_go_to_line(cx: &mut TestAppContext) {
        let root = fixture("find-shortcuts");
        let (workspace, cx) = setup(cx, root.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_editor(Some(root.join("x.txt")), "one one one", None, window, cx)
        });
        let editor = cx.read(|cx| workspace.read(cx).active_editor().unwrap().clone());
        cx.simulate_keystrokes("secondary-f");
        cx.simulate_input("one");
        cx.simulate_keystrokes("f3");
        assert_eq!(cx.read(|cx| editor.read(cx).newest_range()), 4..7);
        cx.simulate_keystrokes("shift-f3");
        assert_eq!(cx.read(|cx| editor.read(cx).newest_range()), 0..3);
        #[cfg(target_os = "macos")]
        {
            cx.simulate_keystrokes("cmd-g");
            assert_eq!(cx.read(|cx| editor.read(cx).newest_range()), 4..7);
            cx.simulate_keystrokes("cmd-shift-g");
            assert_eq!(cx.read(|cx| editor.read(cx).newest_range()), 0..3);
        }
        cx.simulate_keystrokes("escape ctrl-g");
        assert!(cx.read(|cx| workspace.read(cx).modal.is_some()));
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), "one one one");
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
        cx.simulate_keystrokes("end");
        cx.simulate_keystrokes("secondary-\\");
        let right = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        assert_ne!(left, right);
        assert_eq!(cx.read(|cx| ws.read(cx).panes.len()), 2);
        // Type at the start in the right view; the left view's cursor (at the
        // end) moves with the text, and both show the same contents.
        cx.simulate_keystrokes("home");
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
        cx.dispatch_action(crate::editor::MoveToEnd);
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
        cx.dispatch_action(crate::editor::MoveToStart);
        cx.simulate_keystrokes("f12");
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
        cx.simulate_keystrokes("end");
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
        cx.dispatch_action(crate::editor::MoveToEnd);
        cx.simulate_keystrokes("enter");
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
    fn git_diff_keeps_unsaved_buffers_and_restores_editor_focus(cx: &mut TestAppContext) {
        let root = git_fixture("diff-buffer");
        let (workspace, cx) = setup(cx, root.clone());
        cx.executor().allow_parking();
        wait_for(cx, "repository", &|cx| {
            workspace.read(cx).git.read(cx).repo().is_some()
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("a.txt"), None, window, cx)
        });
        wait_for(cx, "file", &|cx| {
            workspace.read(cx).active_editor().is_some()
        });
        cx.simulate_input("unsaved ");
        let before = active_text(&workspace, cx);
        cx.simulate_keystrokes("ctrl-shift-g secondary-alt-d");
        wait_for(cx, "diff", &|cx| {
            workspace
                .read(cx)
                .file_diff
                .as_ref()
                .is_some_and(|view| view.read(cx).model.is_some())
        });
        let changes = cx.read(|cx| {
            workspace
                .read(cx)
                .file_diff
                .as_ref()
                .unwrap()
                .read(cx)
                .model
                .as_ref()
                .unwrap()
                .changes
                .len()
        });
        assert_eq!(changes, 1);
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        cx.simulate_input("cannot edit this snapshot");
        assert_eq!(active_text(&workspace, cx), before);
        cx.simulate_keystrokes("secondary-w");
        assert!(cx.read(|cx| workspace.read(cx).file_diff.is_none()));
        assert_eq!(cx.read(|cx| workspace.read(cx).all_editors().count()), 1);
        cx.simulate_input("more ");
        assert_eq!(active_text(&workspace, cx), "unsaved more one\ntwo\n");
    }

    #[gpui::test]
    fn git_diff_uses_disk_when_a_clean_open_file_is_deleted(cx: &mut TestAppContext) {
        let root = git_fixture("diff-clean-deleted");
        let (workspace, cx) = setup(cx, root.clone());
        cx.executor().allow_parking();
        wait_for(cx, "repository", &|cx| {
            workspace.read(cx).git.read(cx).repo().is_some()
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("a.txt"), None, window, cx)
        });
        wait_for(cx, "file", &|cx| {
            workspace.read(cx).active_editor().is_some()
        });
        assert!(!cx.read(|cx| {
            workspace
                .read(cx)
                .active_editor()
                .unwrap()
                .read(cx)
                .doc(cx)
                .is_dirty()
        }));
        std::fs::remove_file(root.join("a.txt")).unwrap();
        cx.simulate_keystrokes("secondary-alt-d");
        wait_for(cx, "diff", &|cx| {
            workspace
                .read(cx)
                .file_diff
                .as_ref()
                .is_some_and(|view| view.read(cx).model.is_some())
        });
        cx.read(|cx| {
            let model = workspace
                .read(cx)
                .file_diff
                .as_ref()
                .unwrap()
                .read(cx)
                .model
                .as_ref()
                .unwrap();
            assert_eq!(model.changes.len(), 1);
            assert_eq!(model.rows.len(), 2);
            assert!(model.rows.iter().all(|row| row.new.is_none()));
        });
        assert_eq!(active_text(&workspace, cx), "one\ntwo\n");
    }

    #[gpui::test]
    fn git_diff_navigation_refresh_and_open_file(cx: &mut TestAppContext) {
        let root = git_fixture("diff-navigation");
        std::fs::write(
            root.join("a.txt"),
            format!("ONE\ntwo\n{}\n", "extra".repeat(200)),
        )
        .unwrap();
        let (workspace, cx) = setup(cx, root.clone());
        cx.executor().allow_parking();
        wait_for(cx, "repository", &|cx| {
            workspace.read(cx).git.read(cx).repo().is_some()
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_file_diff(root.join("a.txt"), DiffScope::Working, window, cx)
        });
        wait_for(cx, "diff", &|cx| {
            workspace
                .read(cx)
                .file_diff
                .as_ref()
                .is_some_and(|view| view.read(cx).model.is_some())
        });
        let view = cx.read(|cx| workspace.read(cx).file_diff.as_ref().unwrap().clone());
        cx.update(|window, cx| window.focus(&workspace.focus_handle(cx)));
        let position = cx.update(|window, _| {
            gpui::point(
                window.viewport_size().width * 0.75,
                window.viewport_size().height / 2.,
            )
        });
        cx.simulate_click(position, gpui::Modifiers::default());
        assert!(cx.update(|window, cx| view.focus_handle(cx).is_focused(window)));
        cx.simulate_keystrokes("alt-down");
        assert_eq!(cx.read(|cx| view.read(cx).selected_change()), Some(1));
        cx.simulate_keystrokes("alt-up");
        assert_eq!(cx.read(|cx| view.read(cx).selected_change()), Some(0));
        cx.simulate_keystrokes("alt-right");
        assert!(cx.read(|cx| view.read(cx).horizontal_offset()) > px(0.));
        cx.simulate_keystrokes("alt-left");
        assert_eq!(cx.read(|cx| view.read(cx).horizontal_offset()), px(0.));
        // Panning stops exactly at the end of the 1000-character line,
        // measured with the code font rather than an estimate.
        for _ in 0..200 {
            cx.simulate_keystrokes("alt-right");
        }
        let (offset, limit, char_width) = cx.read(|cx| {
            let view = view.read(cx);
            (
                view.horizontal_offset(),
                view.pan_limit(),
                view.char_width(),
            )
        });
        assert!(char_width > px(0.));
        assert!(limit > char_width * 900.);
        assert_eq!(offset, limit);
        cx.simulate_keystrokes("alt-left");
        assert!(cx.read(|cx| view.read(cx).horizontal_offset()) < limit);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        cx.simulate_keystrokes("secondary-r");
        wait_for(cx, "refreshed diff", &|cx| {
            view.read(cx)
                .model
                .as_ref()
                .is_some_and(|model| model.changes.is_empty())
        });
        cx.simulate_keystrokes("enter");
        wait_for(cx, "editor", &|cx| {
            workspace.read(cx).file_diff.is_none() && workspace.read(cx).active_editor().is_some()
        });
        assert_eq!(active_path(&workspace, cx), Some(root.join("a.txt")));
    }

    #[gpui::test]
    fn git_diff_preserves_crlf_when_using_an_open_buffer(cx: &mut TestAppContext) {
        let root = git_fixture("diff-crlf");
        let repo = crate::git::Repo::discover(&root).unwrap();
        std::fs::write(root.join("crlf.txt"), "one\r\ntwo\r\n").unwrap();
        repo.stage(&["crlf.txt"]).unwrap();
        repo.commit("CRLF", false).unwrap();
        let (workspace, cx) = setup(cx, root.clone());
        cx.executor().allow_parking();
        wait_for(cx, "repository", &|cx| {
            workspace.read(cx).git.read(cx).repo().is_some()
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("crlf.txt"), None, window, cx)
        });
        wait_for(cx, "file", &|cx| {
            workspace.read(cx).active_editor().is_some()
        });
        cx.simulate_keystrokes("secondary-alt-d");
        wait_for(cx, "diff", &|cx| {
            workspace
                .read(cx)
                .file_diff
                .as_ref()
                .is_some_and(|view| view.read(cx).model.is_some())
        });
        assert!(cx.read(|cx| {
            workspace
                .read(cx)
                .file_diff
                .as_ref()
                .unwrap()
                .read(cx)
                .model
                .as_ref()
                .unwrap()
                .changes
                .is_empty()
        }));
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

    /// Real processes through the login shell: one service announces a port,
    /// one fails, then Ctrl+C stops the first.
    #[gpui::test]
    fn services_run_stack_ports_failures_and_stop(cx: &mut TestAppContext) {
        use crate::services_panel::Status as ServiceStatus;
        let root = fixture("services");
        std::fs::create_dir_all(root.join(".solder")).unwrap();
        std::fs::write(
            root.join(".solder/services.json"),
            r#"{"services": [
                {"name": "web", "command": "printf 'ready on http://localhost:4321\\n'; sleep 30"},
                {"name": "broken", "command": "echo boom; exit 3"}
            ]}"#,
        )
        .unwrap();
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        let services = cx.read(|cx| ws.read(cx).services.clone());
        wait_for(cx, "detection", &|cx| services.read(cx).specs().len() == 2);

        cx.simulate_keystrokes("secondary-shift-s");
        assert!(cx.read(|cx| ws.read(cx).sidebar == Some(SidebarTab::Services)));
        cx.dispatch_action(RunStack);
        wait_for(cx, "port from the log", &|cx| {
            services.read(cx).ports("web") == [4321]
        });
        wait_for(cx, "failed service", &|cx| {
            services.read(cx).status("broken", cx) == ServiceStatus::Exited(Some(3))
        });
        assert_eq!(
            cx.read(|cx| services.read(cx).status("web", cx)),
            ServiceStatus::Running
        );
        // The failed service keeps its terminal so its output can be read.
        assert_eq!(cx.read(|cx| ws.read(cx).terminals.len()), 2);

        cx.dispatch_action(StopAll);
        wait_for(cx, "web to stop", &|cx| {
            matches!(
                services.read(cx).status("web", cx),
                ServiceStatus::Exited(_)
            )
        });
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

    /// A project whose `.env` names a SQLite database with a `users` table.
    fn sqlite_fixture(name: &str) -> PathBuf {
        let root = fixture(name);
        let path = root.join("dev.db");
        // An empty file is an empty SQLite database; fill it with the driver
        // the app uses.
        std::fs::write(&path, b"").unwrap();
        futures::executor::block_on(async {
            let session = db::Session::connect(db::ConnectionSpec {
                name: "seed".into(),
                engine: db::Engine::Sqlite,
                url: path.display().to_string(),
                source: "test".into(),
                read_only: false,
            })
            .await
            .unwrap();
            for q in [
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
                "INSERT INTO users (name) VALUES ('ada'), ('bob')",
            ] {
                session.query(q.into()).await.unwrap();
            }
        });
        std::fs::write(root.join(".env"), "DATABASE_URL=file:./dev.db\n").unwrap();
        root
    }

    #[gpui::test]
    fn query_files_run_the_statement_under_the_cursor(cx: &mut TestAppContext) {
        use crate::results::State;
        let root = sqlite_fixture("query-file");
        let file = root.join("report.sql");
        let text = "select name from users order by id;\nselect count(*) as total from users;\n";
        std::fs::write(&file, text).unwrap();
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        let (store, results) = cx.read(|cx| {
            let ws = ws.read(cx);
            (ws.database.clone(), ws.results.clone())
        });
        ws.update_in(cx, |ws, window, cx| {
            ws.open_path(file.clone(), None, window, cx)
        });
        // The only SQL connection becomes the file's, with its schema.
        wait_for(cx, "binding", &|cx| {
            store.read(cx).binding(&file) == Some("DATABASE_URL")
        });
        wait_for(cx, "schema", &|cx| {
            store.read(cx).schema_of("DATABASE_URL").is_some()
        });
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());

        // Cursor in the second statement.
        let second = text.find("count").unwrap();
        editor.update(cx, |e, cx| e.select_range(second..second, cx));
        cx.simulate_keystrokes("secondary-enter");
        wait_for(
            cx,
            "count",
            &|cx| matches!(&results.read(cx).state, State::Done(r) if r.rows == vec![vec![db::Value::Int(2)]]),
        );
        assert_eq!(
            cx.read(|cx| results.read(cx).query.to_string()),
            "select count(*) as total from users"
        );
        // Running keeps the keyboard in the editor.
        assert!(cx.update(|window, cx| editor.focus_handle(cx).is_focused(window)));

        // A selection runs as is.
        let first_end = text.find(';').unwrap();
        editor.update(cx, |e, cx| e.select_range(0..first_end, cx));
        cx.simulate_keystrokes("secondary-enter");
        wait_for(
            cx,
            "names",
            &|cx| matches!(&results.read(cx).state, State::Done(r) if r.rows.len() == 2 && r.columns[0].name == "name"),
        );

        // Completion knows the columns of the tables in the statement.
        let end = text.len();
        editor.update(cx, |e, cx| e.select_range(end..end, cx));
        cx.simulate_input("select  from users");
        let column = end + "select ".len();
        editor.update(cx, |e, cx| e.select_range(column..column, cx));
        cx.simulate_input("na");
        cx.run_until_parked();
        let first = cx.read(|cx| {
            editor
                .read(cx)
                .completion
                .as_ref()
                .and_then(|m| m.selected_item().map(|i| i.label.clone()))
        });
        assert_eq!(first.as_deref(), Some("name"));
        cx.simulate_keystrokes("enter");
        assert!(
            cx.read(|cx| editor.read(cx).text(cx))
                .ends_with("select name from users")
        );
    }

    #[gpui::test]
    fn query_file_asks_which_connection_when_there_are_several(cx: &mut TestAppContext) {
        use crate::results::State;
        let root = sqlite_fixture("query-picker");
        std::fs::copy(root.join("dev.db"), root.join("other.db")).unwrap();
        std::fs::create_dir_all(root.join(".solder")).unwrap();
        std::fs::write(
            root.join(".solder/connections.json"),
            r#"{"connections": [{"name": "other", "url": "./other.db"}]}"#,
        )
        .unwrap();
        let file = root.join("q.sql");
        std::fs::write(&file, "select count(*) from users").unwrap();
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root);
        let (store, results) = cx.read(|cx| {
            let ws = ws.read(cx);
            (ws.database.clone(), ws.results.clone())
        });
        ws.update_in(cx, |ws, window, cx| {
            ws.open_path(file.clone(), None, window, cx)
        });
        wait_for(cx, "detection", &|cx| {
            store.read(cx).connections().len() == 2
        });
        assert!(cx.read(|cx| store.read(cx).binding(&file).is_none()));
        wait_for(cx, "editor", &|cx| ws.read(cx).active_editor().is_some());
        cx.simulate_keystrokes("secondary-enter");
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
        cx.simulate_input("oth");
        cx.simulate_keystrokes("enter");
        wait_for(
            cx,
            "result",
            &|cx| matches!(&results.read(cx).state, State::Done(r) if r.rows.len() == 1),
        );
        assert!(cx.read(|cx| store.read(cx).binding(&file) == Some("other")));
        assert_eq!(
            cx.read(|cx| results.read(cx).connection.to_string()),
            "other"
        );
    }

    /// Opens `query` in Results once the connection and its schema are ready.
    fn show_rows(
        ws: &Entity<Workspace>,
        cx: &mut VisualTestContext,
        connection: &'static str,
        query: &'static str,
    ) -> Entity<crate::results::ResultsView> {
        use crate::{database::SchemaState, results::State};
        let (store, results) = cx.read(|cx| {
            let ws = ws.read(cx);
            (ws.database.clone(), ws.results.clone())
        });
        store.update(cx, |s, cx| s.ensure_detected(cx));
        wait_for(cx, "detection", &|cx| store.read(cx).detected());
        store.update(cx, |s, cx| s.ensure_schema(connection, cx));
        wait_for(cx, "schema", &|cx| {
            matches!(
                store.read(cx).connection(connection).map(|c| &c.schema),
                Some(SchemaState::Loaded(_))
            )
        });
        ws.update_in(cx, |ws, window, cx| {
            ws.run_query(connection.into(), query.into(), true, window, cx)
        });
        wait_for(cx, "rows", &|cx| {
            matches!(&results.read(cx).state, State::Done(_))
        });
        results
    }

    fn rows_of(
        results: &Entity<crate::results::ResultsView>,
        cx: &VisualTestContext,
    ) -> Vec<Vec<String>> {
        cx.read(|cx| match &results.read(cx).state {
            crate::results::State::Done(r) => r
                .rows
                .iter()
                .map(|row| row.iter().map(db::Value::display).collect())
                .collect(),
            _ => Vec::new(),
        })
    }

    #[gpui::test]
    fn results_grid_stages_reviews_and_applies_edits(cx: &mut TestAppContext) {
        use crate::results::{ApplyChanges, State};
        let root = sqlite_fixture("grid-edits");
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        let results = show_rows(
            &ws,
            cx,
            "DATABASE_URL",
            "SELECT id, name FROM users ORDER BY id",
        );

        // Edit bob's name: select, Enter, type, Enter.
        cx.simulate_keystrokes("down right enter");
        cx.simulate_input("Bobby");
        cx.simulate_keystrokes("enter");
        // Delete ada's row.
        cx.simulate_keystrokes("up secondary-backspace");
        cx.read(|cx| {
            let r = results.read(cx);
            assert_eq!(
                r.changes.cells.get(&(1, 1)),
                Some(&Some("Bobby".to_string()))
            );
            assert!(r.changes.deleted.contains(&0));
        });
        // Nothing is written before Apply.
        assert_eq!(rows_of(&results, cx), [["1", "ada"], ["2", "bob"]]);

        cx.simulate_keystrokes("secondary-s");
        assert!(cx.read(|cx| results.read(cx).reviewing));
        let statements = results.update(cx, |r, cx| r.statements(cx)).unwrap();
        assert_eq!(
            statements,
            [
                "DELETE FROM \"users\" WHERE \"id\" = 1",
                "UPDATE \"users\" SET \"name\" = 'Bobby' WHERE \"id\" = 2",
            ]
        );
        cx.dispatch_action(ApplyChanges);
        wait_for(cx, "applied", &|cx| {
            let r = results.read(cx);
            r.changes.is_empty() && matches!(&r.state, State::Done(d) if d.rows.len() == 1)
        });
        assert_eq!(rows_of(&results, cx), [["2", "Bobby"]]);

        // A row that changed since it was read stops the whole save.
        cx.simulate_keystrokes("down right enter");
        cx.simulate_input("Rob");
        cx.simulate_keystrokes("enter");
        futures::executor::block_on(async {
            let other = db::Session::connect(db::ConnectionSpec {
                name: "other".into(),
                engine: db::Engine::Sqlite,
                url: root.join("dev.db").display().to_string(),
                source: "test".into(),
                read_only: false,
            })
            .await
            .unwrap();
            other.query("DELETE FROM users".into()).await.unwrap();
        });
        cx.simulate_keystrokes("secondary-s");
        cx.dispatch_action(ApplyChanges);
        wait_for(cx, "conflict", &|cx| {
            results
                .read(cx)
                .notice
                .as_ref()
                .is_some_and(|n| n.contains("matched no row"))
        });
        // Still staged, so it can be discarded or retried.
        assert_eq!(cx.read(|cx| results.read(cx).changes.len()), 1);
    }

    /// Lets background work (database drivers, pickers loading) finish.
    fn settle(cx: &mut VisualTestContext) {
        for _ in 0..30 {
            cx.executor().advance_clock(Duration::from_millis(50));
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[gpui::test]
    fn results_grid_adds_duplicates_and_picks_references(cx: &mut TestAppContext) {
        use crate::results::{ApplyChanges, State};
        let root = sqlite_fixture("grid-rows");
        futures::executor::block_on(async {
            let session = db::Session::connect(db::ConnectionSpec {
                name: "seed".into(),
                engine: db::Engine::Sqlite,
                url: root.join("dev.db").display().to_string(),
                source: "test".into(),
                read_only: false,
            })
            .await
            .unwrap();
            for q in [
                "CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), \
                 title TEXT NOT NULL, status TEXT DEFAULT 'draft')",
                "INSERT INTO posts (user_id, title, status) VALUES (1, 'hello', 'live')",
            ] {
                session.query(q.into()).await.unwrap();
            }
        });
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root);
        let results = show_rows(
            &ws,
            cx,
            "DATABASE_URL",
            "SELECT id, user_id, title, status FROM posts ORDER BY id",
        );

        // A new row starts typing at user_id: id is filled in by SQLite.
        cx.simulate_keystrokes("secondary-n");
        assert_eq!(cx.read(|cx| results.read(cx).selected), Some((1, 1)));
        // user_id references users: Enter picks one of them by name.
        cx.simulate_keystrokes("escape enter");
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
        cx.simulate_input("bob");
        settle(cx);
        cx.simulate_keystrokes("enter");
        assert!(cx.read(|cx| ws.read(cx).modal.is_none()));
        cx.simulate_keystrokes("right enter");
        cx.simulate_input("second");
        cx.simulate_keystrokes("enter");

        // A copy of the first post, and an added row dropped again.
        cx.simulate_keystrokes("up secondary-d escape");
        cx.simulate_keystrokes("secondary-n escape secondary-backspace");

        cx.simulate_keystrokes("secondary-s");
        let statements = results.update(cx, |r, cx| r.statements(cx)).unwrap();
        assert_eq!(
            statements,
            [
                "INSERT INTO \"posts\" (\"user_id\", \"title\") VALUES ('2', 'second')",
                "INSERT INTO \"posts\" (\"user_id\", \"title\", \"status\") VALUES ('1', 'hello', 'live')",
            ]
        );
        cx.dispatch_action(ApplyChanges);
        wait_for(cx, "saved", &|cx| {
            let r = results.read(cx);
            r.changes.is_empty() && matches!(&r.state, State::Done(d) if d.rows.len() == 3)
        });
        assert_eq!(
            rows_of(&results, cx),
            [
                ["1", "1", "hello", "live"],
                ["2", "2", "second", "draft"],
                ["3", "1", "hello", "live"],
            ]
        );
    }

    #[gpui::test]
    fn browsing_pages_filters_sorts_and_follows_references(cx: &mut TestAppContext) {
        use crate::results::State;
        let root = sqlite_fixture("browse");
        futures::executor::block_on(async {
            let session = db::Session::connect(db::ConnectionSpec {
                name: "seed".into(),
                engine: db::Engine::Sqlite,
                url: root.join("dev.db").display().to_string(),
                source: "test".into(),
                read_only: false,
            })
            .await
            .unwrap();
            for q in [
                "CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), \
                 title TEXT, status TEXT)",
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 450) \
                 INSERT INTO posts (user_id, title, status) \
                 SELECT 1 + i % 2, 'post ' || i, CASE WHEN i % 3 = 0 THEN 'live' ELSE 'draft' END FROM n",
            ] {
                session.query(q.into()).await.unwrap();
            }
        });
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root);
        let (store, panel, results) = cx.read(|cx| {
            let ws = ws.read(cx);
            (
                ws.database.clone(),
                ws.database_panel.clone(),
                ws.results.clone(),
            )
        });
        cx.simulate_keystrokes("ctrl-shift-d");
        wait_for(cx, "detection", &|cx| store.read(cx).detected());
        panel.update(cx, |p, cx| p.toggle_connection(0, cx));
        wait_for(cx, "schema", &|cx| {
            store.read(cx).schema_of("DATABASE_URL").is_some()
        });
        // Tables are listed by name: posts, users.
        panel.update(cx, |p, cx| p.open_object(0, 0, cx));
        let loaded = |cx: &App| match &results.read(cx).state {
            State::Done(r) => r.rows.len(),
            _ => 0,
        };
        let total = |cx: &App| results.read(cx).browsing.as_ref().and_then(|b| b.total);
        wait_for(cx, "first page", &|cx| {
            loaded(cx) == 200 && total(cx) == Some(450)
        });

        // Scrolling to the last rows loads the next pages.
        cx.update(|window, cx| window.focus(&results.focus_handle(cx)));
        cx.simulate_keystrokes(&["down"; 199].join(" "));
        wait_for(cx, "second page", &|cx| loaded(cx) == 400);
        cx.simulate_keystrokes(&["down"; 200].join(" "));
        wait_for(cx, "last page", &|cx| {
            loaded(cx) == 450
                && results
                    .read(cx)
                    .browsing
                    .as_ref()
                    .is_some_and(|b| b.exhausted)
        });

        // A typed filter, then narrowing by a cell's value.
        cx.simulate_keystrokes("secondary-f");
        cx.simulate_input("user_id = 2");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "filtered", &|cx| {
            total(cx) == Some(225) && loaded(cx) == 200
        });
        // Row 3 (id 3) is 'live'; alt-f keeps only live posts of user 2.
        cx.simulate_keystrokes("down right right right alt-f");
        wait_for(cx, "narrowed", &|cx| total(cx) == Some(75));
        assert_eq!(
            cx.read(|cx| results.read(cx).query.to_string()),
            "SELECT * FROM \"posts\" WHERE (user_id = 2) AND \"status\" = 'live' ORDER BY \"id\" LIMIT 200"
        );

        // Sorting by title from the header: descending on the second click.
        results.update(cx, |r, cx| r.toggle_sort(2, cx));
        wait_for(cx, "ascending", &|cx| {
            results.read(cx).query.contains("ORDER BY \"title\" LIMIT") && loaded(cx) == 75
        });
        results.update(cx, |r, cx| r.toggle_sort(2, cx));
        wait_for(cx, "sorted", &|cx| {
            results.read(cx).query.contains("ORDER BY \"title\" DESC") && loaded(cx) == 75
        });
        assert_eq!(rows_of(&results, cx)[0][2], "post 99");

        // Following user_id opens that user; Back returns to the posts.
        // Reloading clears the selection: the first row's user_id.
        cx.simulate_keystrokes("right alt-enter");
        wait_for(cx, "user", &|cx| {
            results
                .read(cx)
                .query
                .starts_with("SELECT * FROM \"users\"")
                && loaded(cx) == 1
        });
        assert_eq!(rows_of(&results, cx), [["2", "bob"]]);
        cx.simulate_keystrokes("alt-left");
        wait_for(cx, "back", &|cx| {
            results.read(cx).query.contains("FROM \"posts\"") && loaded(cx) == 75
        });
        assert!(cx.read(|cx| results.read(cx).query.contains("DESC")));
    }

    #[gpui::test]
    fn structure_changes_apply_or_become_migrations(cx: &mut TestAppContext) {
        use crate::structure::StructureView;
        let root = sqlite_fixture("structure");
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        let store = cx.read(|cx| ws.read(cx).database.clone());
        store.update(cx, |s, cx| s.ensure_detected(cx));
        wait_for(cx, "detection", &|cx| store.read(cx).detected());
        store.update(cx, |s, cx| s.ensure_schema("DATABASE_URL", cx));
        wait_for(cx, "schema", &|cx| {
            store.read(cx).schema_of("DATABASE_URL").is_some()
        });
        let table = |cx: &App, name: &str| {
            store
                .read(cx)
                .schema_of("DATABASE_URL")
                .and_then(|(_, s)| s.objects.iter().find(|o| o.name == name).cloned())
        };
        let open =
            |cx: &mut VisualTestContext, object: Option<db::Object>| -> Entity<StructureView> {
                ws.update_in(cx, |ws, window, cx| {
                    ws.open_structure(
                        "DATABASE_URL".into(),
                        db::Engine::Sqlite,
                        object,
                        window,
                        cx,
                    );
                    ws.structure.as_ref().unwrap().0.clone()
                })
            };
        let edit_users = |view: &Entity<StructureView>, cx: &mut VisualTestContext| {
            view.update(cx, |v, cx| {
                v.set_column(1, "full_name", "text", "", cx);
                v.add_column(cx);
                v.set_column(2, "email", "text", "", cx);
                v.add_index(cx);
                v.set_index(0, "users_email", "email", cx);
            });
        };
        let closed = |cx: &App| ws.read(cx).structure.is_none();

        // A draft that cannot work says why instead of reviewing.
        let users = cx.read(|cx| table(cx, "users")).unwrap();
        let view = open(cx, Some(users.clone()));
        view.update(cx, |v, cx| v.add_index(cx));
        cx.simulate_keystrokes("secondary-s");
        cx.run_until_parked();
        cx.read(|cx| {
            let v = view.read(cx);
            assert!(v.review.is_none());
            assert!(
                v.notice
                    .as_ref()
                    .is_some_and(|n| n.contains("needs at least one column")),
                "{:?}",
                v.notice
            );
        });
        cx.simulate_keystrokes("escape");
        assert!(cx.read(|cx| closed(cx)));

        // Reviewed, then saved as a migration: the database is untouched.
        let view = open(cx, Some(users.clone()));
        edit_users(&view, cx);
        cx.simulate_keystrokes("secondary-s");
        wait_for(cx, "review", &|cx| view.read(cx).review.is_some());
        cx.read(|cx| {
            let review = view.read(cx).review.as_ref().unwrap();
            assert_eq!(
                review.up,
                [
                    "ALTER TABLE \"users\" RENAME COLUMN \"name\" TO \"full_name\"",
                    "ALTER TABLE \"users\" ADD COLUMN \"email\" text",
                    "CREATE INDEX \"users_email\" ON \"users\" (\"email\")",
                ]
            );
            assert_eq!(review.target.dir, root.join("migrations"));
        });
        view.update(cx, |v, cx| v.save_migration(cx));
        wait_for(cx, "saved", &|cx| {
            closed(cx) && ws.read(cx).active_editor().is_some()
        });
        let migration = active_path(&ws, cx).unwrap();
        assert!(migration.starts_with(root.join("migrations")));
        assert!(migration.to_string_lossy().ends_with("_alter_users.sql"));
        let text = std::fs::read_to_string(&migration).unwrap();
        assert!(text.contains("ADD COLUMN \"email\" text;"), "{text}");
        assert!(text.contains("-- To undo:"), "{text}");
        assert!(cx.read(|cx| table(cx, "users")).unwrap().columns[1].name == "name");

        // The same change applied to the database.
        let view = open(cx, Some(users));
        edit_users(&view, cx);
        cx.simulate_keystrokes("secondary-s");
        wait_for(cx, "review", &|cx| view.read(cx).review.is_some());
        view.update(cx, |v, cx| v.apply(cx));
        wait_for(cx, "applied", &|cx| {
            closed(cx)
                && table(cx, "users").is_some_and(|t| {
                    t.columns
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        == ["id", "full_name", "email"]
                })
        });

        // A new table, then dropping it.
        let view = open(cx, None);
        view.update(cx, |v, cx| {
            v.set_table_name("tags", cx);
            v.set_column(0, "id", "integer", "", cx);
            v.add_column(cx);
            v.set_column(1, "label", "text", "'none'", cx);
        });
        cx.simulate_keystrokes("secondary-s");
        wait_for(cx, "review", &|cx| view.read(cx).review.is_some());
        view.update(cx, |v, cx| v.apply(cx));
        wait_for(cx, "created", &|cx| {
            closed(cx) && table(cx, "tags").is_some()
        });
        let tags = cx.read(|cx| table(cx, "tags")).unwrap();
        assert!(tags.columns[0].primary_key && tags.columns[0].auto);
        let view = open(cx, Some(tags));
        view.update(cx, |v, cx| {
            v.drop = true;
            cx.notify();
        });
        cx.simulate_keystrokes("secondary-s");
        wait_for(cx, "review", &|cx| view.read(cx).review.is_some());
        assert_eq!(
            cx.read(|cx| view.read(cx).review.as_ref().unwrap().up.clone()),
            ["DROP TABLE \"tags\""]
        );
        view.update(cx, |v, cx| v.apply(cx));
        wait_for(cx, "dropped", &|cx| {
            closed(cx) && table(cx, "tags").is_none()
        });
    }

    #[gpui::test]
    fn erd_moves_opens_links_and_exports(cx: &mut TestAppContext) {
        use crate::erd_view::ErdView;
        use gpui::{Modifiers, MouseButton, MouseDownEvent};
        let root = sqlite_fixture("erd");
        futures::executor::block_on(async {
            let session = db::Session::connect(db::ConnectionSpec {
                name: "seed".into(),
                engine: db::Engine::Sqlite,
                url: root.join("dev.db").display().to_string(),
                source: "test".into(),
                read_only: false,
            })
            .await
            .unwrap();
            session
                .query(
                    "CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), title TEXT)"
                        .into(),
                )
                .await
                .unwrap();
        });
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        let store = cx.read(|cx| ws.read(cx).database.clone());
        store.update(cx, |s, cx| s.ensure_detected(cx));
        wait_for(cx, "detection", &|cx| store.read(cx).detected());
        let view: Entity<ErdView> = ws.update_in(cx, |ws, window, cx| {
            ws.open_erd("DATABASE_URL".into(), db::Engine::Sqlite, window, cx);
            ws.erd.as_ref().unwrap().0.clone()
        });
        wait_for(cx, "diagram", &|cx| view.read(cx).diagram.edges.len() == 1);
        settle(cx);
        let node = |cx: &VisualTestContext, name: &str| {
            cx.read(|cx| {
                let d = &view.read(cx).diagram;
                d.nodes.iter().position(|n| n.name == name).unwrap()
            })
        };
        let (users, posts) = (node(cx, "users"), node(cx, "posts"));
        let at = |cx: &VisualTestContext, i: usize, dx: f32, dy: f32| {
            cx.read(|cx| {
                let v = view.read(cx);
                let n = &v.diagram.nodes[i];
                v.to_window((n.x + dx, n.y + dy))
            })
        };
        let x_of =
            |cx: &VisualTestContext, i: usize| cx.read(|cx| view.read(cx).diagram.nodes[i].x);
        // Referenced tables sit to the left.
        assert!(x_of(cx, users) < x_of(cx, posts));

        // Dragging a table by its header moves it and saves the layout.
        let before = x_of(cx, users);
        let start = at(cx, users, 20., 10.);
        let end = gpui::point(start.x + px(60.), start.y + px(30.));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        let moved = x_of(cx, users);
        assert!(moved > before + 10., "{before} -> {moved}");
        let layout = db::erd::layout_path(&root, "DATABASE_URL");
        wait_for(cx, "layout saved", &|_| layout.exists());
        let saved = db::erd::Layout::load(&layout);
        assert_eq!(saved.tables["users"].x, moved);

        // Dragging from posts.title's handle onto users.name drafts a key.
        let (w, title_y) = cx.read(|cx| {
            let n = &view.read(cx).diagram.nodes[posts];
            (n.width, n.row_y("title") - n.y)
        });
        let from = at(cx, posts, w - 4., title_y);
        let name_y = cx.read(|cx| {
            let n = &view.read(cx).diagram.nodes[users];
            n.row_y("name") - n.y
        });
        let onto = at(cx, users, 30., name_y);
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(onto, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(onto, MouseButton::Left, Modifiers::default());
        let structure = cx
            .read(|cx| ws.read(cx).structure.as_ref().map(|(v, _)| v.clone()))
            .unwrap();
        let draft = cx.read(|cx| structure.read(cx).draft(cx));
        assert_eq!(draft.name, "posts");
        let fk = draft
            .foreign_keys
            .iter()
            .find(|f| f.name == "posts_title_fkey")
            .unwrap();
        assert_eq!(
            (
                fk.columns.clone(),
                fk.ref_table.as_str(),
                fk.ref_columns.clone()
            ),
            (vec!["title".to_string()], "users", vec!["name".to_string()])
        );
        // Closing the form returns to the diagram.
        cx.simulate_keystrokes("escape");
        assert!(cx.read(|cx| ws.read(cx).structure.is_none() && ws.read(cx).erd.is_some()));

        // Double-clicking a table browses its rows.
        cx.simulate_event(MouseDownEvent {
            position: at(cx, users, 20., 10.),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
            click_count: 2,
            first_mouse: false,
        });
        let results = cx.read(|cx| ws.read(cx).results.clone());
        wait_for(cx, "browse", &|cx| {
            results.read(cx).query.contains("FROM \"users\"") && ws.read(cx).dock_open
        });

        // Search: Enter selects and centers the first match.
        let search = cx.read(|cx| view.read(cx).search_field());
        cx.update(|window, cx| window.focus(&search.focus_handle(cx)));
        cx.simulate_input("pos");
        cx.simulate_keystrokes("enter");
        assert_eq!(cx.read(|cx| view.read(cx).selected), Some(posts));

        // The exports.
        cx.update(|window, cx| window.focus(&view.focus_handle(cx)));
        view.update(cx, |v, cx| v.copy_mermaid(cx));
        let mermaid = cx.read_from_clipboard().and_then(|i| i.text()).unwrap();
        assert!(
            mermaid.contains("users ||--o{ posts : \"user_id\""),
            "{mermaid}"
        );
        let (svg, png) = (root.join("diagram.svg"), root.join("diagram.png"));
        // The test executor runs these as it is driven, not on their own.
        view.update(cx, |v, cx| v.export_svg(svg.clone(), cx))
            .detach();
        view.update(cx, |v, cx| v.export_png(png.clone(), cx))
            .detach();
        wait_for(cx, "exports", &|_| {
            std::fs::read(&png).is_ok_and(|b| b.len() > 8) && svg.exists()
        });
        assert!(
            std::fs::read_to_string(&svg)
                .unwrap()
                .contains(">posts</text>")
        );
        assert_eq!(&std::fs::read(&png).unwrap()[..4], b"\x89PNG");
        cx.simulate_keystrokes("escape");
        assert!(cx.read(|cx| ws.read(cx).erd.is_none()));
    }

    /// A project whose `.env` points at the test server in `var`, if set.
    fn server_fixture(name: &str, var: &str, key: &str) -> Option<PathBuf> {
        let url = std::env::var(var).ok().filter(|u| !u.is_empty())?;
        let root = fixture(name);
        std::fs::write(root.join(".env"), format!("{key}={url}\n")).unwrap();
        Some(root)
    }

    fn server_session(root: &Path, key: &str) -> db::Session {
        let spec = db::detect(root, None)
            .into_iter()
            .find(|s| s.name == key)
            .unwrap();
        futures::executor::block_on(db::Session::connect(spec)).unwrap()
    }

    #[gpui::test]
    fn redis_keys_edit_expire_rename_delete_and_create(cx: &mut TestAppContext) {
        use crate::results::{ApplyChanges, KeyAction, ResultsEvent, State};
        let Some(root) = server_fixture("redis-keys", "SOLDER_TEST_REDIS", "REDIS_URL") else {
            return;
        };
        let redis = server_session(&root, "REDIS_URL");
        let run = |q: &str| futures::executor::block_on(redis.query(q.into())).unwrap();
        run("DEL solder:ws:h solder:ws:h2 solder:ws:list");
        run("HSET solder:ws:h name ada");
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root);
        let (store, results, panel) = cx.read(|cx| {
            let ws = ws.read(cx);
            (
                ws.database.clone(),
                ws.results.clone(),
                ws.database_panel.clone(),
            )
        });
        store.update(cx, |s, cx| s.ensure_detected(cx));
        wait_for(cx, "detection", &|cx| store.read(cx).detected());
        ws.update_in(cx, |ws, window, cx| {
            ws.run_query(
                "REDIS_URL".into(),
                "HGETALL solder:ws:h".into(),
                true,
                window,
                cx,
            )
        });
        wait_for(cx, "hash", &|cx| results.read(cx).key_ttl == Some(-1));

        // Change a value and add a field, as one transaction.
        cx.simulate_keystrokes("right enter");
        cx.simulate_input("Ada");
        cx.simulate_keystrokes("enter secondary-n");
        cx.simulate_input("lang");
        cx.simulate_keystrokes("enter right enter");
        cx.simulate_input("rust");
        cx.simulate_keystrokes("enter secondary-s");
        assert_eq!(
            results.update(cx, |r, cx| r.statements(cx)).unwrap(),
            ["HSET solder:ws:h name Ada", "HSET solder:ws:h lang rust"]
        );
        cx.dispatch_action(ApplyChanges);
        wait_for(cx, "saved", &|cx| {
            matches!(&results.read(cx).state, State::Done(r) if r.rows.len() == 2)
                && results.read(cx).changes.is_empty()
        });

        // Expire and rename through the prompts.
        results.update(cx, |_, cx| {
            cx.emit(ResultsEvent::KeyPrompt {
                action: KeyAction::Expire,
                key: "solder:ws:h".into(),
            })
        });
        cx.simulate_input("500");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "expiry", &|cx| {
            results.read(cx).key_ttl.is_some_and(|t| t > 400)
        });
        results.update(cx, |_, cx| {
            cx.emit(ResultsEvent::KeyPrompt {
                action: KeyAction::Rename,
                key: "solder:ws:h".into(),
            })
        });
        cx.simulate_input("solder:ws:h2");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "renamed", &|cx| {
            results.read(cx).query.as_ref() == "HGETALL solder:ws:h2"
                && matches!(&results.read(cx).state, State::Done(r) if r.rows.len() == 2)
        });
        results.update(cx, |r, cx| {
            r.key_command("DEL solder:ws:h2".into(), None, cx)
        });
        wait_for(cx, "deleted", &|cx| {
            matches!(results.read(cx).state, State::Empty)
        });
        assert_eq!(run("EXISTS solder:ws:h2").rows[0][0], db::Value::Int(0));

        // A new list: pick the type, fill the first item, apply.
        panel.update(cx, |_, cx| {
            cx.emit(crate::database_panel::DatabasePanelEvent::NewKey {
                connection: "REDIS_URL".into(),
            })
        });
        cx.simulate_input("solder:ws:list");
        cx.simulate_keystrokes("down down enter");
        wait_for(cx, "new key", &|cx| {
            results.read(cx).changes.inserted.len() == 1
        });
        cx.update(|window, cx| window.focus(&results.focus_handle(cx)));
        cx.simulate_keystrokes("enter");
        cx.simulate_input("first");
        cx.simulate_keystrokes("enter secondary-s");
        cx.dispatch_action(ApplyChanges);
        wait_for(cx, "list saved", &|cx| results.read(cx).changes.is_empty());
        assert_eq!(
            run("LRANGE solder:ws:list 0 -1").rows[0][1],
            db::Value::Text("first".into())
        );
        run("DEL solder:ws:list");
    }

    #[gpui::test]
    fn mongo_documents_edit_delete_and_insert(cx: &mut TestAppContext) {
        use crate::results::{ApplyChanges, State};
        let Some(root) = server_fixture("mongo-docs", "SOLDER_TEST_MONGO", "MONGO_URL") else {
            return;
        };
        let mongo = server_session(&root, "MONGO_URL");
        let run = |q: &str| futures::executor::block_on(mongo.query(q.into())).unwrap();
        run("db.solder_ws_docs.deleteMany({})");
        run(
            "db.solder_ws_docs.insertMany([{_id: 1, name: 'ada', age: 36}, {_id: 2, name: 'bob', age: 25}])",
        );
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root);
        let (store, results) = cx.read(|cx| {
            let ws = ws.read(cx);
            (ws.database.clone(), ws.results.clone())
        });
        store.update(cx, |s, cx| s.ensure_detected(cx));
        wait_for(cx, "detection", &|cx| store.read(cx).detected());
        let spec = db::browse::Browse {
            table: "solder_ws_docs".into(),
            key: vec!["_id".into()],
            ..Default::default()
        };
        results.update(cx, |r, cx| {
            r.browse("MONGO_URL".into(), db::Engine::Mongo, spec, cx)
        });
        wait_for(
            cx,
            "documents",
            &|cx| matches!(&results.read(cx).state, State::Done(r) if r.rows.len() == 2),
        );
        // As opening it from the Database tab does: show the Results tab.
        ws.update(cx, |ws, cx| {
            ws.show_results = true;
            ws.results_active = true;
            ws.response_active = false;
            ws.dock_open = true;
            cx.notify();
        });
        cx.update(|window, cx| window.focus(&results.focus_handle(cx)));
        cx.simulate_keystrokes("right enter");
        cx.simulate_input("Ada");
        cx.simulate_keystrokes("enter down secondary-backspace secondary-n");
        cx.simulate_input("cy");
        cx.simulate_keystrokes("enter secondary-s");
        assert_eq!(
            results.update(cx, |r, cx| r.statements(cx)).unwrap(),
            [
                "db.getCollection(\"solder_ws_docs\").updateOne({\"_id\": 1}, {$set: {\"name\": \"Ada\"}})",
                "db.getCollection(\"solder_ws_docs\").deleteOne({\"_id\": 2})",
                "db.getCollection(\"solder_ws_docs\").insertOne({\"name\": \"cy\"})",
            ]
        );
        cx.dispatch_action(ApplyChanges);
        wait_for(cx, "saved", &|cx| {
            results.read(cx).changes.is_empty()
                && matches!(&results.read(cx).state, State::Done(r) if r.rows.len() == 2)
        });
        let names: Vec<String> =
            run("db.solder_ws_docs.find({}, {_id: 0, name: 1}).sort({name: 1})")
                .rows
                .iter()
                .map(|r| r[0].display())
                .collect();
        assert_eq!(names, ["Ada", "cy"]);
        run("db.solder_ws_docs.deleteMany({})");
    }

    #[gpui::test]
    fn connection_actions_appear_on_hover(cx: &mut TestAppContext) {
        let root = sqlite_fixture("panel-hover");
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root);
        let store = cx.read(|cx| ws.read(cx).database.clone());
        cx.simulate_keystrokes("ctrl-shift-d");
        wait_for(cx, "detection", &|cx| store.read(cx).detected());
        cx.run_until_parked();
        // Hidden until the row is hovered: not drawn, so not clickable.
        assert!(cx.debug_bounds("db-erd-0").is_none());
        let row = cx.debug_bounds("db-connection-0").unwrap();
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        let button = cx.debug_bounds("db-erd-0").expect("shown on hover");
        assert!(row.contains(&button.center()));
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        assert!(cx.read(|cx| ws.read(cx).erd.is_some()));
    }

    #[gpui::test]
    fn read_only_results_need_unlocking(cx: &mut TestAppContext) {
        use crate::results::ApplyChanges;
        let root = sqlite_fixture("grid-locked");
        std::fs::copy(root.join("dev.db"), root.join("prod.db")).unwrap();
        std::fs::create_dir_all(root.join(".solder")).unwrap();
        std::fs::write(
            root.join(".solder/connections.json"),
            r#"{"connections": [{"name": "prod", "url": "./prod.db", "readOnly": true}]}"#,
        )
        .unwrap();
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root);
        let results = show_rows(&ws, cx, "prod", "SELECT id, name FROM users ORDER BY id");
        cx.simulate_keystrokes("down right enter");
        assert!(
            cx.read(|cx| results.read(cx).notice.clone())
                .is_some_and(|n| n.contains("read-only"))
        );

        let store = cx.read(|cx| ws.read(cx).database.clone());
        cx.update(|window, cx| {
            crate::database::confirm_unlock(store.clone(), "prod".into(), window, cx)
        });
        cx.simulate_prompt_answer("Allow changes");
        wait_for(cx, "unlocked", &|cx| {
            store
                .read(cx)
                .connection("prod")
                .is_some_and(|c| !c.locked())
        });
        let results = show_rows(&ws, cx, "prod", "SELECT id, name FROM users ORDER BY id");
        cx.simulate_keystrokes("down right enter");
        cx.simulate_input("Bobby");
        cx.simulate_keystrokes("enter secondary-s");
        cx.dispatch_action(ApplyChanges);
        wait_for(cx, "saved", &|cx| {
            results.read(cx).changes.is_empty() && results.read(cx).notice.is_none()
        });
        wait_for(
            cx,
            "reloaded",
            &|cx| matches!(&results.read(cx).state, crate::results::State::Done(r) if r.rows.len() == 2),
        );
        assert_eq!(rows_of(&results, cx), [["1", "ada"], ["2", "Bobby"]]);
    }

    #[gpui::test]
    fn database_tab_previews_tables_in_results(cx: &mut TestAppContext) {
        use crate::{database::SchemaState, results::State};
        let root = sqlite_fixture("database");
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root);
        let (store, panel, results) = cx.read(|cx| {
            let ws = ws.read(cx);
            (
                ws.database.clone(),
                ws.database_panel.clone(),
                ws.results.clone(),
            )
        });
        // Nothing is read until the tab is opened.
        assert!(!cx.read(|cx| store.read(cx).detected()));
        cx.simulate_keystrokes("ctrl-shift-d");
        assert!(cx.read(|cx| ws.read(cx).sidebar == Some(SidebarTab::Database)));
        wait_for(cx, "detection", &|cx| store.read(cx).detected());
        // The file named in .env and found on disk is one connection.
        assert_eq!(cx.read(|cx| store.read(cx).connections().len()), 1);

        panel.update(cx, |p, cx| p.toggle_connection(0, cx));
        wait_for(cx, "schema", &|cx| {
            matches!(
                store.read(cx).connections()[0].schema,
                SchemaState::Loaded(_)
            )
        });
        panel.update(cx, |p, cx| p.open_object(0, 0, cx));
        wait_for(
            cx,
            "preview rows",
            &|cx| matches!(&results.read(cx).state, State::Done(r) if r.rows.len() == 2),
        );
        cx.read(|cx| {
            let ws = ws.read(cx);
            assert!(ws.dock_open && ws.show_results && ws.results_active);
        });

        cx.update(|window, cx| window.focus(&results.focus_handle(cx)));
        cx.simulate_keystrokes("down right secondary-c");
        assert_eq!(cx.read(|cx| results.read(cx).selected), Some((1, 1)));
        assert_eq!(
            cx.read_from_clipboard().and_then(|i| i.text()).as_deref(),
            Some("bob")
        );

        ws.update_in(cx, |ws, window, cx| {
            ws.run_query(
                "DATABASE_URL".into(),
                "SELECT nope FROM users".into(),
                true,
                window,
                cx,
            )
        });
        wait_for(
            cx,
            "error",
            &|cx| matches!(&results.read(cx).state, State::Failed(e) if e.contains("nope")),
        );
        ws.update_in(cx, |ws, window, cx| ws.close_results(window, cx));
        assert!(!cx.read(|cx| ws.read(cx).dock_open));
    }

    /// A server that answers every request with its method, path and body
    /// as JSON. Returns its port.
    fn echo_server() -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut data = Vec::new();
                let mut buf = [0; 4096];
                let (head, body) = loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break (String::new(), String::new());
                    }
                    data.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&data).to_string();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length: usize = head
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse().ok())?
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break (head.to_string(), body.to_string());
                        }
                    }
                };
                let mut line = head.lines().next().unwrap_or("").split(' ');
                let (method, path) = (line.next().unwrap_or(""), line.next().unwrap_or(""));
                let json = format!(
                    "{{\"method\":\"{method}\",\"path\":\"{path}\",\"body\":{}}}",
                    serde_json::to_string(&body).unwrap()
                );
                let status = if method == "POST" {
                    "201 Created"
                } else {
                    "200 OK"
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
            }
        });
        port
    }

    fn response_json(
        ws: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> (u16, serde_json::Value) {
        use crate::response::ResponseState;
        wait_for(cx, "the response", &|cx| {
            !matches!(
                ws.read(cx).response.read(cx).state,
                ResponseState::Sending | ResponseState::Empty
            )
        });
        cx.read(|cx| match &ws.read(cx).response.read(cx).state {
            ResponseState::Done(response, _) => (
                response.status,
                serde_json::from_slice(&response.body).unwrap(),
            ),
            ResponseState::Failed(e) => panic!("request failed: {e}"),
            _ => unreachable!(),
        })
    }

    #[gpui::test]
    fn http_file_sends_the_request_under_the_cursor(cx: &mut TestAppContext) {
        let port = echo_server();
        let root = db::testing::dir("ws-http-file");
        std::fs::write(root.join(".env"), format!("PORT={port}\n")).unwrap();
        let file = root.join("api.http");
        std::fs::write(
            &file,
            "@base = http://127.0.0.1:{{PORT}}\n\n### list\nGET {{base}}/users\n\n### create\nPOST {{base}}/users\nContent-Type: application/json\n\n{\"name\": \"Ada\"}\n",
        )
        .unwrap();
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        ws.update_in(cx, |w, window, cx| w.open_path(file, None, window, cx));
        cx.run_until_parked();
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let at = active_text(&ws, cx).find("POST").unwrap();
        editor.update(cx, |e, cx| {
            e.select_ranges(std::slice::from_ref(&(at..at)), cx)
        });
        cx.simulate_keystrokes("secondary-enter");
        let (status, json) = response_json(&ws, cx);
        assert_eq!(status, 201);
        assert_eq!(json["method"], "POST");
        assert_eq!(json["path"], "/users");
        assert_eq!(json["body"], "{\"name\": \"Ada\"}");
        assert!(cx.read(|cx| ws.read(cx).response_active && ws.read(cx).dock_open));

        // A request that names a variable nobody defines says which one.
        let editor_text = active_text(&ws, cx);
        editor.update(cx, |e, cx| {
            let end = editor_text.len();
            e.select_ranges(std::slice::from_ref(&(end..end)), cx)
        });
        cx.simulate_input("\n### broken\nGET {{missing}}/x\n");
        cx.simulate_keystrokes("secondary-enter");
        cx.run_until_parked();
        cx.read(|cx| match &ws.read(cx).response.read(cx).state {
            crate::response::ResponseState::Failed(e) => assert!(e.contains("missing"), "{e}"),
            _ => panic!("expected an error"),
        });
    }

    #[gpui::test]
    fn api_tab_turns_routes_into_requests(cx: &mut TestAppContext) {
        let port = echo_server();
        let root = db::testing::dir("ws-api-tab");
        std::fs::create_dir_all(root.join("app/api/users")).unwrap();
        std::fs::write(
            root.join("app/api/users/route.ts"),
            "export async function GET() {}\nexport async function POST() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("server.js"),
            "app.get('/health', (req, res) => res.send('ok'))\n",
        )
        .unwrap();
        std::fs::write(
            root.join("openapi.yaml"),
            "openapi: 3.1.0\ninfo: {title: Pets, version: '1'}\nservers: [{url: 'http://127.0.0.1:1'}]\npaths:\n  /pets:\n    get: {summary: List pets}\n",
        )
        .unwrap();
        std::fs::write(root.join(".env"), format!("PORT={port}\n")).unwrap();
        let root = root.canonicalize().unwrap();
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        let scratch = root.join("scratch");
        ws.update(cx, |w, _| w.scratch_dir = scratch.clone());
        let panel = cx.read(|cx| ws.read(cx).api_panel.clone());
        cx.simulate_keystrokes("ctrl-shift-h");
        wait_for(cx, "routes", &|cx| panel.read(cx).loaded);
        let rows: Vec<(String, String)> = cx.read(|cx| {
            panel
                .read(cx)
                .visible_rows()
                .into_iter()
                .map(|r| (r.method, r.path))
                .collect()
        });
        for (method, path) in [
            ("GET", "/api/users"),
            ("POST", "/api/users"),
            ("GET", "/health"),
            ("GET", "/pets"),
        ] {
            assert!(
                rows.contains(&(method.into(), path.into())),
                "{method} {path} in {rows:?}"
            );
        }

        let filter = cx.read(|cx| panel.read(cx).filter_field());
        cx.update(|window, cx| window.focus(&filter.focus_handle(cx)));
        cx.simulate_input("health");
        cx.run_until_parked();
        let rows = cx.read(|cx| panel.read(cx).visible_rows());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "/health");

        // Clicking a route adds its request to the requests file.
        let row = cx.debug_bounds("route-0").unwrap();
        cx.simulate_click(row.center(), gpui::Modifiers::default());
        let requests = cx.read(|cx| ws.read(cx).requests_path(cx));
        wait_for(cx, "the requests file", &|cx| {
            ws.read(cx)
                .active_editor()
                .and_then(|e| e.read(cx).path(cx).map(Path::to_path_buf))
                .as_deref()
                == Some(requests.as_path())
        });
        let text = active_text(&ws, cx);
        assert!(
            text.contains(&format!("@baseUrl = http://localhost:{port}")),
            "{text}"
        );
        assert!(text.contains("GET {{baseUrl}}/health"), "{text}");

        // The cursor is on the new request, so cmd-enter sends it.
        cx.simulate_keystrokes("secondary-enter");
        let (status, json) = response_json(&ws, cx);
        assert_eq!(status, 200);
        assert_eq!(json["path"], "/health");

        // A second route goes to the end of the open file.
        cx.update(|window, cx| window.focus(&filter.focus_handle(cx)));
        cx.simulate_keystrokes("secondary-a backspace");
        cx.simulate_input("post users");
        cx.run_until_parked();
        let row = cx.debug_bounds("route-0").unwrap();
        cx.simulate_click(row.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let text = active_text(&ws, cx);
        assert!(text.contains("GET {{baseUrl}}/health"), "{text}");
        assert!(text.trim_end().ends_with("{}"), "{text}");
        assert!(text.contains("POST {{baseUrl}}/api/users"), "{text}");

        // Send, shown on hover, sends without touching the file.
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        let send = cx.debug_bounds("route-send-0").expect("shown on hover");
        cx.simulate_click(send.center(), gpui::Modifiers::default());
        let (status, json) = response_json(&ws, cx);
        assert_eq!(status, 201);
        assert_eq!(json["path"], "/api/users");
        assert_eq!(json["body"], "{}");
        assert_eq!(active_text(&ws, cx), text);
    }

    #[test]
    fn openapi_import_writes_a_http_file() {
        let root = db::testing::dir("ws-openapi");
        let spec = root.join("petstore.json");
        std::fs::write(
            &spec,
            r#"{"openapi":"3.1.0","info":{"title":"Pets","version":"1"},
               "servers":[{"url":"https://api.example.com/v1"}],
               "paths":{"/pets/{petId}":{"get":{"summary":"One pet",
                 "parameters":[{"name":"petId","in":"path","schema":{"type":"integer"}}]}}}}"#,
        )
        .unwrap();
        let out = import_openapi_file(&spec).unwrap();
        assert_eq!(out, root.join("petstore.http"));
        let text = std::fs::read_to_string(out).unwrap();
        assert!(
            text.contains("@baseUrl = https://api.example.com/v1"),
            "{text}"
        );
        assert!(text.contains("GET {{baseUrl}}/pets/"), "{text}");
        let swagger = root.join("old.json");
        std::fs::write(&swagger, r#"{"swagger":"2.0","paths":{}}"#).unwrap();
        assert!(import_openapi_file(&swagger).is_err());
    }

    /// A stand-in for Hugging Face: `HEAD` answers like the hub's redirect,
    /// with the file's size and SHA-256; `GET` serves the file.
    fn fake_hub(body: Vec<u8>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let sha = std::process::Command::new("python3")
            .args([
                "-c",
                "import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child.stdin.take().unwrap().write_all(&body)?;
                child.wait_with_output()
            })
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                if first.starts_with("HEAD") {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 302 Found\r\nLocation: /cdn\r\nX-Linked-Size: {}\r\nX-Linked-Etag: \"{sha}\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                } else {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(&body);
                }
            }
        });
        base
    }

    /// The smallest GGUF header Solder reads: an architecture and one tensor.
    fn tiny_gguf() -> Vec<u8> {
        let mut out = b"GGUF".to_vec();
        out.extend(3u32.to_le_bytes());
        out.extend(1u64.to_le_bytes());
        out.extend(1u64.to_le_bytes());
        let string = |out: &mut Vec<u8>, s: &str| {
            out.extend((s.len() as u64).to_le_bytes());
            out.extend(s.as_bytes());
        };
        string(&mut out, "general.architecture");
        out.extend(8u32.to_le_bytes());
        string(&mut out, "llama");
        string(&mut out, "output.weight");
        out.extend(2u32.to_le_bytes());
        out.extend(1000u64.to_le_bytes());
        out.extend(1000u64.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend(0u64.to_le_bytes());
        out
    }

    #[gpui::test]
    fn ai_tab_benchmarks_installs_and_assigns_models(cx: &mut TestAppContext) {
        use crate::ai_store::AiStore;
        let root = db::testing::dir("ws-ai");
        let data = root.join("data");
        // The pinned server build, already installed: a mock that reports
        // fixed timings.
        let bin = data.join("llama").join(ai::install::LLAMA_BUILD);
        std::fs::create_dir_all(&bin).unwrap();
        let mock =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../ai/tests/fixtures/mock_llama_server.py");
        std::fs::copy(mock, bin.join("llama-server")).unwrap();
        let hub = fake_hub(b"GGUF calibration model".to_vec());
        // A model LM Studio downloaded.
        let lm_studio = root.join("lmstudio");
        std::fs::create_dir_all(lm_studio.join("org/Tiny-GGUF")).unwrap();
        std::fs::write(
            lm_studio.join("org/Tiny-GGUF/Tiny-Q4_K_M.gguf"),
            tiny_gguf(),
        )
        .unwrap();
        cx.executor().allow_parking();
        let store = cx.update(|cx| {
            let store = cx.new(|_| {
                AiStore::new(ai::Dirs::new(&data), hub.clone())
                    .with_scan_dirs(vec![lm_studio.clone()])
            });
            AiStore::set_global(store.clone(), cx);
            store
        });
        let (ws, cx) = setup(cx, root.clone());
        cx.simulate_keystrokes("ctrl-shift-a");
        wait_for(cx, "hardware", &|cx| store.read(cx).hardware.is_some());
        cx.run_until_parked();

        let run = cx.debug_bounds("ai-benchmark").expect("benchmark button");
        cx.simulate_click(run.center(), gpui::Modifiers::default());
        wait_for(cx, "the benchmark", &|cx| store.read(cx).measured.is_some());
        let calibration = ai::catalog::calibration();
        let calibration_file = ai::Dirs::new(&data).model(calibration);
        cx.read(|cx| {
            let s = store.read(cx);
            assert_eq!(
                s.measured,
                Some(ai::Speed {
                    prompt: 1234.5,
                    generate: 67.8
                })
            );
            assert!(s.installed.contains(&calibration.id));
            assert!(s.error.is_none(), "{:?}", s.error);
            // The LM Studio model is listed and runnable where it is.
            assert_eq!(s.found.len(), 1);
            assert!(s.installed.contains(&s.found[0].id));
            assert_eq!(s.found[0].name, "Tiny-Q4_K_M");
        });
        assert_eq!(
            std::fs::read(&calibration_file).unwrap(),
            b"GGUF calibration model"
        );

        // Installed models can be given a role.
        cx.run_until_parked();
        assert_eq!(calibration.id, "qwen3.5-0.8b");
        let toggle = cx
            .debug_bounds("ai-role-chat-qwen3.5-0.8b")
            .expect("role toggle");
        cx.simulate_click(toggle.center(), gpui::Modifiers::default());
        assert_eq!(
            cx.read(|cx| store.read(cx).roles.get(&ai::Role::Chat).cloned()),
            Some(crate::ai_providers::ModelRef::local(&calibration.id))
        );
        settle(cx);

        // The measurement and the role survive a restart.
        let again = cx.update(|_, cx| cx.new(|_| AiStore::new(ai::Dirs::new(&data), hub.clone())));
        again.update(cx, |s, cx| s.load(cx));
        wait_for(cx, "the saved state", &|cx| {
            again.read(cx).hardware.is_some()
        });
        cx.read(|cx| {
            let s = again.read(cx);
            assert!(s.measured.is_some());
            assert_eq!(
                s.roles.get(&ai::Role::Chat),
                Some(&crate::ai_providers::ModelRef::local(&calibration.id))
            );
        });

        // Delete shows on hover and removes the file and its role; the
        // calibration model is the first row. (gpui never clears debug
        // bounds between frames, so whether it was hidden before cannot be
        // read back here; the Database tab's test covers that.)
        let row = cx.debug_bounds("ai-model-0").unwrap();
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        let button = cx
            .debug_bounds("ai-delete-qwen3.5-0.8b")
            .expect("shown on hover");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        wait_for(cx, "the file to go", &|_| !calibration_file.exists());
        cx.read(|cx| {
            let s = store.read(cx);
            assert!(!s.installed.contains(&calibration.id));
            assert!(s.roles.is_empty());
        });

        // A .gguf path typed into the field is added, described by its
        // header, and kept across restarts.
        let own = root.join("mine/Own-Q8_0.gguf");
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, tiny_gguf()).unwrap();
        let panel = cx.read(|cx| ws.read(cx).ai_panel.clone());
        let field = cx.read(|cx| panel.read(cx).add_field());
        cx.update(|window, cx| window.focus(&field.focus_handle(cx)));
        cx.simulate_input(&own.display().to_string());
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the model to be added", &|cx| {
            !store.read(cx).custom.is_empty()
        });
        cx.read(|cx| {
            let s = store.read(cx);
            assert!(s.error.is_none(), "{:?}", s.error);
            let added = &s.custom[0];
            assert_eq!(added.name, "Own-Q8_0");
            assert_eq!(added.params, 0.001);
            assert!(s.installed.contains(&added.id));
            assert!(s.candidates().iter().any(|c| c.model.id == added.id));
        });
        assert!(cx.read(|cx| field.read(cx).text(cx)).is_empty());
        settle(cx);
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(data.join("ai.json")).unwrap()).unwrap();
        assert_eq!(state["custom"][0]["name"], "Own-Q8_0");

        // A bad entry says why.
        cx.simulate_input("not a model");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the error", &|cx| store.read(cx).error.is_some());
    }

    /// An OpenAI- and Anthropic-style API on localhost: lists `model-a`, and
    /// streams back "Echo: <the question's last line>" for chats. Records
    /// each request body. Returns the base URL (with `/v1`).
    fn fake_api() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap_or(0);
                let mut length = 0;
                let mut auth = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                    if lower.starts_with("authorization:") || lower.starts_with("x-api-key:") {
                        auth = line.trim().to_string();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).ok();
                let body = String::from_utf8_lossy(&body).to_string();
                log.lock()
                    .unwrap()
                    .push(format!("{} {auth}\n{body}", first.trim()));
                if first.starts_with("GET") {
                    let json = r#"{"data":[{"id":"model-a"}]}"#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    continue;
                }
                let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let question = value["messages"]
                    .as_array()
                    .and_then(|m| m.last())
                    .and_then(|m| m["content"].as_str())
                    .and_then(|c| c.lines().last())
                    .unwrap_or("")
                    .to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
                );
                let anthropic = first.contains("/messages");
                for word in format!("Echo: {question}").split(' ') {
                    let data = if anthropic {
                        serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text": format!("{word} ")}})
                    } else {
                        serde_json::json!({"choices":[{"delta":{"content": format!("{word} ")}}]})
                    };
                    let _ = write!(stream, "data: {data}\n\n");
                    let _ = stream.flush();
                }
                if !anthropic {
                    let _ = write!(stream, "data: [DONE]\n\n");
                }
            }
        });
        (base, seen)
    }

    fn ai_setup<'a>(
        cx: &'a mut TestAppContext,
        name: &str,
        urls: crate::ai_providers::Urls,
    ) -> (
        PathBuf,
        Entity<crate::ai_store::AiStore>,
        Entity<Workspace>,
        &'a mut VisualTestContext,
    ) {
        use crate::ai_store::AiStore;
        let root = fixture(name);
        let data = root.join("data");
        cx.executor().allow_parking();
        let store = cx.update(|cx| {
            let store = cx.new(|_| {
                AiStore::new(ai::Dirs::new(&data), "http://127.0.0.1:9".into())
                    .with_scan_dirs(Vec::new())
                    .for_tests(urls)
            });
            AiStore::set_global(store.clone(), cx);
            store
        });
        let (ws, cx) = setup(cx, root.clone());
        (root, store, ws, cx)
    }

    fn down_urls() -> crate::ai_providers::Urls {
        // Nothing listens on port 9: local providers are "not running". The
        // network ones have no key, so nothing is sent to them; OpenAI's
        // address is off this machine, for offline mode to turn it off.
        crate::ai_providers::Urls {
            ollama: "http://127.0.0.1:9/v1".into(),
            lm_studio: "http://127.0.0.1:9/v1".into(),
            anthropic: "http://127.0.0.1:9/v1".into(),
            openai: "https://api.openai.invalid/v1".into(),
        }
    }

    fn chat_answer(ws: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
        let chat = cx.read(|cx| ws.read(cx).chat.clone());
        wait_for(cx, "the answer", &|cx| {
            let c = chat.read(cx);
            !c.streaming() && c.messages.last().is_some_and(|m| m.done)
        });
        cx.read(|cx| {
            let m = chat.read(cx).messages.last().unwrap();
            assert!(m.error.is_none(), "{:?}", m.error);
            m.text.trim().to_string()
        })
    }

    #[gpui::test]
    fn chat_with_an_added_provider_and_the_open_file(cx: &mut TestAppContext) {
        let (api, seen) = fake_api();
        let (root, store, ws, cx) = ai_setup(cx, "chat-provider", down_urls());
        ws.update_in(cx, |w, window, cx| {
            w.open_path(root.join("src/main.rs"), None, window, cx)
        });
        cx.run_until_parked();

        // Add a provider in the AI tab's providers view.
        cx.simulate_keystrokes("ctrl-shift-a");
        let panel = cx.read(|cx| ws.read(cx).ai_panel.clone());
        panel.update(cx, |p, cx| p.show(crate::ai_panel::View::Providers, cx));
        wait_for(cx, "the built-in providers", &|cx| {
            store
                .read(cx)
                .provider("ollama")
                .is_some_and(|p| matches!(p.status, crate::ai_providers::Status::Unavailable(_)))
        });
        cx.read(|cx| {
            let s = store.read(cx);
            assert_eq!(
                s.provider("anthropic").unwrap().status,
                crate::ai_providers::Status::NoKey
            );
        });
        let open = cx.debug_bounds("ai-new-provider").unwrap();
        cx.simulate_click(open.center(), gpui::Modifiers::default());
        cx.simulate_input("Proxy");
        let fields = cx.read(|cx| {
            let p = panel.read(cx);
            (p.provider_url_field(), p.provider_key_field())
        });
        cx.update(|window, cx| window.focus(&fields.0.focus_handle(cx)));
        cx.simulate_input(&api);
        cx.update(|window, cx| window.focus(&fields.1.focus_handle(cx)));
        cx.simulate_input("sk-proxy");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the provider", &|cx| {
            store
                .read(cx)
                .provider("custom-proxy")
                .is_some_and(|p| p.models() == ["model-a"])
        });
        assert_eq!(
            cx.read(|cx| store.read(cx).provider("custom-proxy").unwrap().key_source),
            Some("saved")
        );

        // Open the chat, pick the model, ask.
        cx.update(|window, cx| window.focus(&ws.focus_handle(cx)));
        cx.simulate_keystrokes("secondary-shift-l");
        cx.run_until_parked();
        let pick = cx.debug_bounds("chat-model").unwrap();
        cx.simulate_click(pick.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let model = cx
            .debug_bounds("chat-model-custom-proxy-model-a")
            .expect("in the picker");
        cx.simulate_click(model.center(), gpui::Modifiers::default());
        let chat = cx.read(|cx| ws.read(cx).chat.clone());
        let input = cx.read(|cx| chat.read(cx).input());
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("What does main do?");
        cx.simulate_keystrokes("enter");
        assert_eq!(chat_answer(&ws, cx), "Echo: What does main do?");
        cx.read(|cx| {
            let c = chat.read(cx);
            assert_eq!(c.messages[0].context.as_deref(), Some("src/main.rs"));
        });
        let request = seen.lock().unwrap().last().unwrap().clone();
        assert!(
            request.starts_with("POST /v1/chat/completions "),
            "{request}"
        );
        assert!(
            request.contains("authorization: Bearer sk-proxy"),
            "{request}"
        );
        assert!(
            request.contains("helper();"),
            "the file goes with the question: {request}"
        );
        assert!(request.contains(r#""model":"model-a""#));

        // A follow-up carries the conversation; without the file it is shorter.
        let toggle = cx.debug_bounds("chat-file").unwrap();
        cx.simulate_click(toggle.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("And then?");
        cx.simulate_keystrokes("enter");
        assert_eq!(chat_answer(&ws, cx), "Echo: And then?");
        let request = seen.lock().unwrap().last().unwrap().clone();
        assert!(request.contains("Echo: What does main do?"), "{request}");
        assert_eq!(request.matches("helper();").count(), 1, "{request}");
        assert_eq!(cx.read(|cx| chat.read(cx).messages.len()), 4);
        assert_eq!(cx.read(|cx| input.read(cx).text(cx)), "");

        // Offline mode turns network providers off; this one is local.
        let offline = cx.debug_bounds("ai-offline").unwrap();
        cx.simulate_click(offline.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.read(|cx| {
            let s = store.read(cx);
            assert!(s.offline);
            assert_eq!(
                s.provider("openai").unwrap().status,
                crate::ai_providers::Status::Offline
            );
        });
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("data/ai.json")).unwrap()).unwrap();
        assert_eq!(saved["providers"][0]["name"], "Proxy");
        assert_eq!(saved["roles"]["chat"]["provider"], "custom-proxy");
        assert_eq!(saved["offline"], true);
        let keys = std::fs::read_to_string(root.join("data/keys.json")).unwrap();
        assert!(keys.contains("sk-proxy") && !saved.to_string().contains("sk-proxy"));
    }

    #[gpui::test]
    fn chat_never_attaches_env_files_or_their_selections(cx: &mut TestAppContext) {
        let (api, seen) = fake_api();
        let urls = crate::ai_providers::Urls {
            lm_studio: api,
            ..down_urls()
        };
        let (root, store, ws, cx) = ai_setup(cx, "chat-private-context", urls);
        let secret = "API_KEY=private-context-test-marker\n";
        let paths = [
            ".env",
            ".ENV.local",
            "config/.env.production",
            ".envrc",
            ".env.d/settings.json",
        ];
        for path in paths {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, secret).unwrap();
        }
        cx.simulate_keystrokes("secondary-shift-l");
        wait_for(cx, "the local provider", &|cx| {
            store
                .read(cx)
                .provider("lmstudio")
                .is_some_and(|provider| provider.models() == ["model-a"])
        });
        store.update(cx, |store, cx| {
            store.choose(
                ai::Role::Chat,
                crate::ai_providers::ModelRef {
                    provider: "lmstudio".into(),
                    model: "model-a".into(),
                },
                cx,
            );
        });
        let chat = cx.read(|cx| ws.read(cx).chat.clone());
        let input = cx.read(|cx| chat.read(cx).input());
        for path in paths {
            let path = root.join(path);
            ws.update_in(cx, |workspace, window, cx| {
                workspace.open_path(path.clone(), None, window, cx);
            });
            wait_for(cx, "the private file", &|cx| {
                ws.read(cx)
                    .active_editor()
                    .is_some_and(|editor| editor.read(cx).path(cx) == Some(path.as_path()))
            });
            let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
            for range in [0..0, 0..secret.len()] {
                editor.update(cx, |editor, cx| editor.select_range(range, cx));
                assert!(cx.read(|cx| ws.read(cx).file_context(cx).is_none()));
                cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
                cx.simulate_input("Explain the settings");
                cx.simulate_keystrokes("enter");
                assert_eq!(chat_answer(&ws, cx), "Echo: Explain the settings");
                cx.read(|cx| {
                    let messages = &chat.read(cx).messages;
                    assert!(messages.iter().all(|message| message.context.is_none()));
                    assert!(
                        messages
                            .iter()
                            .all(|message| !message.prompt.contains("private-context-test-marker"))
                    );
                });
            }
        }
        let requests = seen.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            paths.len() * 2
        );
        assert!(
            requests
                .iter()
                .all(|request| !request.contains("private-context-test-marker"))
        );
        drop(requests);
        let source = root.join("src/main.rs");
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(source.clone(), None, window, cx);
        });
        wait_for(cx, "the source file", &|cx| {
            ws.read(cx)
                .active_editor()
                .is_some_and(|editor| editor.read(cx).path(cx) == Some(source.as_path()))
        });
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let start = cx.read(|cx| editor.read(cx).text(cx).find("helper();").unwrap());
        editor.update(cx, |editor, cx| {
            editor.select_range(start..start + "helper();".len(), cx)
        });
        cx.read(|cx| {
            let context = ws.read(cx).file_context(cx).unwrap();
            assert_eq!(context.text, "helper();");
            assert_eq!(context.part.as_deref(), Some("line 2"));
        });
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("Explain the selection");
        cx.simulate_keystrokes("enter");
        assert_eq!(chat_answer(&ws, cx), "Echo: Explain the selection");
        let requests = seen.lock().unwrap();
        let request = requests.last().unwrap();
        assert!(request.contains("helper();") && !request.contains("fn main()"));
        assert!(!request.contains("private-context-test-marker"));
    }

    #[gpui::test]
    fn chat_with_anthropic_after_adding_a_key(cx: &mut TestAppContext) {
        let (api, seen) = fake_api();
        let urls = crate::ai_providers::Urls {
            anthropic: api,
            ..down_urls()
        };
        let (_root, store, ws, cx) = ai_setup(cx, "chat-anthropic", urls);
        cx.simulate_keystrokes("ctrl-shift-a");
        let panel = cx.read(|cx| ws.read(cx).ai_panel.clone());
        panel.update(cx, |p, cx| p.show(crate::ai_panel::View::Providers, cx));
        wait_for(cx, "no key yet", &|cx| {
            store.read(cx).provider("anthropic").unwrap().status
                == crate::ai_providers::Status::NoKey
        });
        cx.run_until_parked();
        let add = cx.debug_bounds("ai-key-anthropic").unwrap();
        cx.simulate_click(add.center(), gpui::Modifiers::default());
        cx.simulate_input("sk-ant-test");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the models", &|cx| {
            store.read(cx).provider("anthropic").unwrap().models() == ["model-a"]
        });
        store.update(cx, |s, cx| {
            s.choose(
                ai::Role::Chat,
                crate::ai_providers::ModelRef {
                    provider: "anthropic".into(),
                    model: "model-a".into(),
                },
                cx,
            )
        });
        cx.simulate_keystrokes("secondary-shift-l");
        cx.simulate_input("Hi there");
        cx.simulate_keystrokes("enter");
        assert_eq!(chat_answer(&ws, cx), "Echo: Hi there");
        let request = seen.lock().unwrap().last().unwrap().clone();
        assert!(request.starts_with("POST /v1/messages "), "{request}");
        assert!(request.contains("x-api-key: sk-ant-test"), "{request}");

        // Forgetting the key leaves the provider without one.
        let forget = cx.debug_bounds("ai-forget-key-anthropic").unwrap();
        cx.simulate_click(forget.center(), gpui::Modifiers::default());
        wait_for(cx, "the key to go", &|cx| {
            store.read(cx).provider("anthropic").unwrap().status
                == crate::ai_providers::Status::NoKey
        });
        let chat = cx.read(|cx| ws.read(cx).chat.clone());
        let input = cx.read(|cx| chat.read(cx).input());
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("Again?");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the refusal", &|cx| {
            chat.read(cx)
                .messages
                .last()
                .is_some_and(|m| m.error.is_some())
        });
    }

    #[gpui::test]
    fn chat_with_a_local_model_starts_its_server(cx: &mut TestAppContext) {
        let (root, store, ws, cx) = ai_setup(cx, "chat-local", down_urls());
        let data = root.join("data");
        let bin = data.join("llama").join(ai::install::LLAMA_BUILD);
        std::fs::create_dir_all(&bin).unwrap();
        let mock =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../ai/tests/fixtures/mock_llama_server.py");
        std::fs::copy(mock, bin.join("llama-server")).unwrap();
        let model = ai::catalog::calibration();
        let file = ai::Dirs::new(&data).model(model);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "GGUF").unwrap();
        store.update(cx, |s, cx| s.load(cx));
        wait_for(cx, "the install", &|cx| {
            store.read(cx).installed.contains(&model.id)
        });
        store.update(cx, |s, cx| s.set_role(ai::Role::Chat, &model.id, cx));

        cx.simulate_keystrokes("secondary-shift-l");
        cx.simulate_input("Explain this");
        cx.simulate_keystrokes("enter");
        assert_eq!(chat_answer(&ws, cx), "Local answer to: Explain this");
        cx.read(|cx| {
            let s = store.read(cx);
            assert!(s.local.as_ref().is_some_and(|r| r.model == model.id));
        });
        // Removing the model stops its server.
        let model = model.clone();
        store.update(cx, |s, cx| s.remove(model, cx));
        assert!(cx.read(|cx| store.read(cx).local.is_none()));
    }
}
