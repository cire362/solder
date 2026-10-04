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
    inline_edit::{InlineEdit, InlineEditEvent},
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
        ShowPlugins,
        ShowExtensions,
        ImportSettings,
        ToggleChat,
        ShowAgent,
        DebugStart,
        DebugPick,
        DebugStop,
        DebugStepOver,
        DebugStepIn,
        DebugStepOut,
        ShowInlineEdit,
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
        KeyBinding::new("secondary-shift-i", ShowAgent, None),
        KeyBinding::new("f5", DebugStart, None),
        KeyBinding::new("shift-f5", DebugStop, None),
        KeyBinding::new("f10", DebugStepOver, None),
        KeyBinding::new("f11", DebugStepIn, None),
        KeyBinding::new("shift-f11", DebugStepOut, None),
        KeyBinding::new(
            "secondary-i",
            ShowInlineEdit,
            Some("Editor && mode == full"),
        ),
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

const SIDEBAR_WIDTH: f32 = 390.;

/// The bottom dock's tab in front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockView {
    Terminal,
    Results,
    Response,
    Debug,
}
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
    Extensions,
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
    extensions_panel: Entity<crate::extensions_panel::ExtensionsPanel>,
    chat: Entity<crate::chat_panel::ChatPanel>,
    agent: Entity<crate::agent_panel::AgentPanel>,
    /// The right dock shows the agent rather than the chat.
    agent_shown: bool,
    /// Pushes started, for tests: the terminal running one may be gone.
    #[cfg(test)]
    pushes: usize,
    chat_open: bool,
    inline_edit: Option<(Entity<InlineEdit>, Subscription)>,
    results: Entity<ResultsView>,
    /// The Results tab is in the dock (a query has run and it was not closed).
    show_results: bool,
    /// What the dock shows: the active terminal or one of its other tabs.
    dock_view: DockView,
    /// The last HTTP response, the dock's Response tab.
    response: Entity<crate::response::ResponseView>,
    show_response: bool,
    plugins: Entity<crate::plugin_store::PluginStore>,
    debug: Entity<crate::debug::DebugStore>,
    debug_panel: Entity<crate::debug_panel::DebugPanel>,
    show_debug: bool,
    /// Scratch queries and the requests file; tests point it elsewhere.
    scratch_dir: PathBuf,
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
        #[cfg(not(test))]
        Self::offer_import(window, cx);
        let plugins = crate::plugin_store::PluginStore::global(cx);
        let this = cx.entity().downgrade();
        plugins.update(cx, |p, cx| p.set_workspace(this, root.clone(), cx));
        let debug = crate::debug::DebugStore::global(cx);
        let debug_panel =
            cx.new(|cx| crate::debug_panel::DebugPanel::new(debug.clone(), root.clone(), cx));
        let ai_store = crate::ai_store::AiStore::global(cx);
        ai_store.update(cx, |s, _| s.add_root(root.clone()));
        let ai_panel = cx.new(|cx| crate::ai_panel::AiPanel::new(ai_store.clone(), cx));
        let extensions = crate::extension_store::ExtensionStore::global(cx);
        let extensions_panel =
            cx.new(|cx| crate::extensions_panel::ExtensionsPanel::new(extensions, cx));
        let weak = cx.entity().downgrade();
        let chat =
            cx.new(|cx| crate::chat_panel::ChatPanel::new(ai_store.clone(), weak.clone(), cx));
        let agent =
            cx.new(|cx| crate::agent_panel::AgentPanel::new(ai_store, weak, root.clone(), cx));
        let results = cx.new(|cx| ResultsView::new(database.clone(), cx));
        let response = cx.new(crate::response::ResponseView::new);
        let project_search = cx.new(|cx| ProjectSearch::new(root, window, cx));
        let search_bar = cx.new(|cx| BufferSearchBar::new(window, cx));
        let subscriptions = vec![
            // Plugins' status texts are drawn in this window's status bar.
            cx.observe(&plugins, |_, _, cx| cx.notify()),
            // Plugins read and change the editor of the window in front.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    let (weak, root) = (cx.entity().downgrade(), this.root(cx));
                    this.plugins
                        .update(cx, |p, cx| p.set_workspace(weak, root, cx));
                }
            }),
            cx.subscribe_in(&debug, window, |this, _, event, window, cx| match event {
                crate::debug::DebugEvent::Paused(path, line) => {
                    // The window whose project holds the file shows it.
                    if path.starts_with(this.root(cx)) {
                        this.show_debug = true;
                        this.dock_view = DockView::Debug;
                        this.dock_open = true;
                        this.open_path(
                            path.clone(),
                            Some(Jump::Point {
                                row: line.saturating_sub(1) as usize,
                                column: 0,
                            }),
                            window,
                            cx,
                        );
                        // A browser being debugged covers the editor; a
                        // pause brings it back in front.
                        cx.activate(true);
                        window.activate_window();
                    }
                }
                crate::debug::DebugEvent::Reveal(path, line) => {
                    if path.starts_with(this.root(cx)) {
                        this.open_path(
                            path.clone(),
                            Some(Jump::Point {
                                row: line.saturating_sub(1) as usize,
                                column: 0,
                            }),
                            window,
                            cx,
                        );
                    }
                }
            }),
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
                        this.dock_view = DockView::Results;
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
                                        ws.dock_view = DockView::Results;
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
                    GitPanelEvent::OpenAt(path, line) => this.open_path(
                        path.clone(),
                        Some(Jump::Point {
                            row: line.saturating_sub(1) as usize,
                            column: 0,
                        }),
                        window,
                        cx,
                    ),
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
            extensions_panel,
            chat,
            agent,
            agent_shown: false,
            #[cfg(test)]
            pushes: 0,
            chat_open: false,
            inline_edit: None,
            results,
            show_results: false,
            dock_view: DockView::Terminal,
            response,
            show_response: false,
            plugins,
            debug,
            debug_panel,
            show_debug: false,
            scratch_dir: settings::config_dir().join("scratch"),
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

    pub(crate) fn active_editor(&self) -> Option<&Entity<Editor>> {
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
                EditorEvent::ShowCodeActions { actions } => {
                    if actions.is_empty() {
                        return;
                    }
                    let picker = CodeActionPicker::new(editor.clone(), actions.clone());
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
                    this.discard_inline_edit(cx);
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
        if self
            .inline_edit
            .as_ref()
            .is_some_and(|(edit, _)| !edit.read(cx).is_target(&editor))
        {
            self.discard_inline_edit(cx);
        }
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
        if self
            .inline_edit
            .as_ref()
            .is_some_and(|(edit, _)| edit.read(cx).is_target(editor))
        {
            self.discard_inline_edit(cx);
        }
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
        self.discard_inline_edit(cx);
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
        #[cfg(test)]
        {
            self.pushes += 1;
        }
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
        self.dock_view = DockView::Terminal;
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
            self.dock_view = DockView::Terminal;
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
        if tab == Some(SidebarTab::Extensions) {
            self.extensions_panel.update(cx, |p, cx| p.shown(cx));
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
        self.dock_view = DockView::Results;
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
        self.discard_inline_edit(cx);
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
        self.discard_inline_edit(cx);
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
                        this.dock_view = DockView::Results;
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

    fn discard_inline_edit(&mut self, cx: &mut Context<Self>) {
        if let Some((panel, _)) = self.inline_edit.take() {
            panel.update(cx, |panel, cx| panel.abort(cx));
        }
    }

    fn show_inline_edit(
        &mut self,
        _: &ShowInlineEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.active_editor().cloned() else {
            return;
        };
        self.close_file_diff(window, cx);
        if self.inline_edit.is_none() {
            let root = self.root(cx);
            let store = crate::ai_store::AiStore::global(cx);
            let panel = cx.new(|cx| InlineEdit::new(editor, root, store, cx));
            let subscription = cx.subscribe_in(&panel, window, |this, _, event, window, cx| {
                match event {
                    InlineEditEvent::Close => {
                        this.discard_inline_edit(cx);
                        if let Some(editor) = this.active_editor() {
                            window.focus(&editor.focus_handle(cx));
                        }
                    }
                    InlineEditEvent::ChooseModel => {
                        this.show_ai(&ShowAi, window, cx);
                        this.ai_panel.update(cx, |panel, cx| {
                            panel.show(crate::ai_panel::View::Providers, cx)
                        });
                    }
                }
                cx.notify();
            });
            self.inline_edit = Some((panel, subscription));
        }
        let input = self.inline_edit.as_ref().unwrap().0.read(cx).input();
        window.focus(&input.focus_handle(cx));
        cx.notify();
    }

    fn show_agent(&mut self, _: &ShowAgent, window: &mut Window, cx: &mut Context<Self>) {
        self.show_right(true, window, cx);
    }

    /// Opens the right dock on the chat or the agent and focuses its field.
    fn show_right(&mut self, agent: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_open = true;
        self.agent_shown = agent;
        let input = if agent {
            self.agent.update(cx, |a, cx| a.shown(cx));
            self.agent.read(cx).input()
        } else {
            self.chat.update(cx, |c, cx| c.shown(cx));
            self.chat.read(cx).input()
        };
        window.focus(&input.focus_handle(cx));
        cx.notify();
    }

    /// Shows a file an agent changed: as it was when the task started, and
    /// as it is in the agent's worktree.
    pub fn show_agent_diff(
        &mut self,
        path: PathBuf,
        rel: String,
        old: String,
        new: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.discard_inline_edit(cx);
        let view = cx.new(|cx| FileDiff::preview(path, cx));
        view.update(cx, |view, cx| {
            view.set_result(
                Ok(DiffModel::new(crate::git::FileDiffSnapshot {
                    path: rel,
                    old,
                    new,
                    can_open: false,
                })),
                cx,
            )
        });
        self.diff_subscription =
            Some(
                cx.subscribe_in(&view, window, |this, _, event, window, cx| {
                    if let FileDiffEvent::Close = event {
                        this.close_file_diff(window, cx);
                    }
                }),
            );
        self.diff_task = None;
        self.file_diff = Some(view);
        cx.notify();
    }

    fn toggle_chat(&mut self, _: &ToggleChat, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent_shown && self.chat_open {
            return self.show_right(false, window, cx);
        }
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
        let (source, path, part) = self.file_context_meta(cx)?;
        let editor = self.active_editor()?.read(cx);
        let range = editor.newest_range();
        let rope = editor.rope(cx);
        let range = if part.is_some() {
            range
        } else {
            0..rope.len_bytes()
        };
        Some(crate::chat_panel::FileContext {
            source,
            path,
            text: rope
                .byte_slice(range)
                .chars()
                .take(crate::chat_panel::FILE_LIMIT + 1)
                .collect(),
            part,
        })
    }

    pub fn file_context_label(&self, cx: &App) -> Option<String> {
        let (_, path, part) = self.file_context_meta(cx)?;
        Some(match part {
            Some(part) => format!("{path} ({part})"),
            None => path,
        })
    }

    fn file_context_meta(&self, cx: &App) -> Option<(PathBuf, String, Option<String>)> {
        let editor = self.active_editor()?.read(cx);
        let source = editor.path(cx)?.to_path_buf();
        let path = &source;
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
        if range.is_empty() || range.end > editor.rope(cx).len_bytes() {
            return Some((source, path, None));
        }
        let buffer = editor.buf(cx);
        let first = buffer.offset_to_point(range.start).row + 1;
        let last = buffer.offset_to_point(range.end).row + 1;
        Some((
            source,
            path,
            Some(if first == last {
                format!("line {first}")
            } else {
                format!("lines {first}-{last}")
            }),
        ))
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
        self.dock_view = DockView::Response;
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

    /// The tab in front once another closes.
    fn fallback_dock_view(&self) -> DockView {
        if self.show_debug {
            DockView::Debug
        } else if self.show_results {
            DockView::Results
        } else if self.show_response {
            DockView::Response
        } else {
            DockView::Terminal
        }
    }

    fn dock_has_tabs(&self) -> bool {
        !self.terminals.is_empty() || self.show_results || self.show_response || self.show_debug
    }

    // ---------------------------------------------------------------- debug

    /// The Debug tab in front, with what can be debugged read again for the
    /// file in front; `start` then runs the chosen configuration.
    fn show_debug_tab(&mut self, start: bool, cx: &mut Context<Self>) {
        let file = self
            .active_editor()
            .and_then(|e| e.read(cx).path(cx).map(Path::to_path_buf));
        self.debug_panel
            .update(cx, |p, cx| p.refresh_configs(file, start, cx));
        self.show_debug = true;
        self.dock_view = DockView::Debug;
        self.dock_open = true;
        cx.notify();
    }

    fn import_settings(&mut self, _: &ImportSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.show_import(
            crate::import_settings::system_roots(),
            settings::config_dir(),
            window,
            cx,
        );
    }

    /// The Import window for the editors under `roots`, writing to `target`.
    fn show_import(
        &mut self,
        roots: import::Roots,
        target: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_modal(window, cx, move |_, cx| {
            crate::import_view::ImportView::new(roots, target, cx)
        });
    }

    /// On a first launch with another editor's settings here, shows the
    /// Import window once. The look is off the UI thread, after the window
    /// is up.
    #[cfg(not(test))]
    fn offer_import(window: &mut Window, cx: &mut Context<Self>) {
        static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        // Once per run, and never under a benchmark, which types into the
        // editor.
        if ASKED.swap(true, std::sync::atomic::Ordering::Relaxed)
            || std::env::vars_os()
                .any(|(name, _)| name.to_string_lossy().starts_with("SOLDER_BENCH"))
        {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let offer = cx
                .background_executor()
                .spawn(async {
                    crate::import_settings::first_launch_offer(
                        &settings::config_dir(),
                        &crate::import_settings::system_roots(),
                    )
                })
                .await;
            if offer {
                this.update_in(cx, |this, window, cx| {
                    this.import_settings(&ImportSettings, window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    fn show_plugins(&mut self, _: &ShowPlugins, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.plugins.clone();
        self.toggle_modal(window, cx, move |_, cx| {
            crate::plugins_view::PluginsView::new(store, cx)
        });
    }

    fn show_extensions(&mut self, _: &ShowExtensions, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(SidebarTab::Extensions), cx);
        window.focus(&self.extensions_panel.focus_handle(cx));
    }

    /// F5: start what is chosen (the file in front first), or continue.
    fn debug_start(&mut self, _: &DebugStart, _: &mut Window, cx: &mut Context<Self>) {
        if self.debug.read(cx).state.active() {
            self.debug_panel.update(cx, |p, cx| p.start_or_continue(cx));
            return;
        }
        self.show_debug_tab(true, cx);
    }

    fn debug_pick(&mut self, _: &DebugPick, _: &mut Window, cx: &mut Context<Self>) {
        self.show_debug_tab(false, cx);
        self.debug_panel.update(cx, |p, cx| p.toggle_picker(cx));
    }

    fn debug_stop(&mut self, _: &DebugStop, _: &mut Window, cx: &mut Context<Self>) {
        self.debug.update(cx, |s, cx| s.stop(cx));
    }

    fn debug_over(&mut self, _: &DebugStepOver, _: &mut Window, cx: &mut Context<Self>) {
        self.debug.update(cx, |s, cx| s.step_over(cx));
    }

    fn debug_in(&mut self, _: &DebugStepIn, _: &mut Window, cx: &mut Context<Self>) {
        self.debug.update(cx, |s, cx| s.step_in(cx));
    }

    fn debug_out(&mut self, _: &DebugStepOut, _: &mut Window, cx: &mut Context<Self>) {
        self.debug.update(cx, |s, cx| s.step_out(cx));
    }

    fn close_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_debug = false;
        self.dock_view = self.fallback_dock_view();
        if !self.dock_has_tabs() {
            self.dock_open = false;
        }
        let _ = window;
        cx.notify();
    }

    fn render_debug_tab(
        &self,
        theme: &crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.dock_view == DockView::Debug;
        let paused = self.debug.read(cx).state == crate::debug::State::Paused;
        div()
            .id("debug-tab")
            .debug_selector(|| "debug-tab".into())
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
            .on_click(cx.listener(|this, _, _, cx| {
                this.dock_view = DockView::Debug;
                cx.notify();
            }))
            .when(paused, |d| {
                d.child(div().size(px(6.)).rounded(px(3.)).bg(theme.warning))
            })
            .child("Debug")
            .child(
                div()
                    .id("debug-close")
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .hover(|d| d.bg(theme.line))
                    .child("×")
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_debug(window, cx);
                    })),
            )
    }

    fn close_response(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was_focused = self.response.focus_handle(cx).contains_focused(window, cx);
        self.show_response = false;
        self.dock_view = self.fallback_dock_view();
        if !self.dock_has_tabs() {
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
        self.dock_view = DockView::Response;
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
        self.dock_view = self.fallback_dock_view();
        if !self.dock_has_tabs() {
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
            self.dock_open = self.dock_open && self.dock_has_tabs();
            self.dock_view = self.fallback_dock_view();
            self.active_terminal = 0;
        } else {
            self.active_terminal = self.active_terminal.min(self.terminals.len() - 1);
        }
        if was_focused {
            match (
                self.terminals.get(self.active_terminal),
                self.active_editor(),
            ) {
                _ if self.dock_view == DockView::Results => {
                    window.focus(&self.results.focus_handle(cx))
                }
                _ if self.dock_view == DockView::Response => {
                    window.focus(&self.response.focus_handle(cx))
                }
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
                    && self.dock_view == DockView::Terminal
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
                self.dock_view = DockView::Terminal;
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
                let active = ix == self.active_terminal && self.dock_view == DockView::Terminal;
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
                        this.dock_view = DockView::Terminal;
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
                    .when(self.show_debug, |d| {
                        d.child(self.render_debug_tab(&theme, cx))
                    })
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
                if self.dock_view == DockView::Debug {
                    d.child(self.debug_panel.clone())
                } else if self.dock_view == DockView::Response {
                    d.child(self.response.clone())
                } else if self.dock_view == DockView::Results {
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
        let active = self.dock_view == DockView::Results;
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
                this.dock_view = DockView::Results;
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
        let active = self.dock_view == DockView::Response;
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
                this.dock_view = DockView::Response;
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
                self.discard_inline_edit(cx);
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
        let plugins = self.plugins.clone();
        let palette = CommandPalette::new(plugins, window, cx);
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
            .when(is_active_pane, |pane| {
                pane.children(self.inline_edit.as_ref().map(|(panel, _)| panel.clone()))
            })
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
                .debug_selector(move || id.to_string())
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
                    SidebarTab::Extensions => this.show_extensions(&ShowExtensions, window, cx),
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
                    .child(tab_button("sidebar-ai", "AI", SidebarTab::Ai))
                    .child(tab_button(
                        "sidebar-extensions",
                        "Extensions",
                        SidebarTab::Extensions,
                    )),
            )
            .child(div().flex_1().min_h_0().pt_1().map(|d| match tab {
                SidebarTab::Files => d.child(self.project_panel.clone()),
                SidebarTab::Search => d.child(self.project_search.clone()),
                SidebarTab::Git => d.child(self.git_panel.clone()),
                SidebarTab::Services => d.child(self.services.clone()),
                SidebarTab::Database => d.child(self.database_panel.clone()),
                SidebarTab::Api => d.child(self.api_panel.clone()),
                SidebarTab::Ai => d.child(self.ai_panel.clone()),
                SidebarTab::Extensions => d.child(self.extensions_panel.clone()),
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
        // What plugins show, and a notice when one is slow; both open the
        // Plugins window.
        let plugins = self.plugins.read(cx);
        let mut plugin_items: Vec<(String, bool)> = plugins
            .status
            .values()
            .map(|text| (text.to_string(), false))
            .collect();
        match plugins.slow().as_slice() {
            [] => {}
            [name] => plugin_items.push((format!("Slow plugin: {name}"), true)),
            names => plugin_items.push((format!("{} slow plugins", names.len()), true)),
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
                    .children(
                        plugin_items
                            .into_iter()
                            .enumerate()
                            .map(|(i, (text, slow))| {
                                div()
                                    .id(("status-plugin", i))
                                    .text_color(if slow { theme.warning } else { theme.fg_muted })
                                    .hover(|d| d.text_color(theme.fg))
                                    .child(text)
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(ShowPlugins), cx)
                                    })
                            }),
                    )
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
            .on_action(cx.listener(Self::show_agent))
            .on_action(cx.listener(Self::show_plugins))
            .on_action(cx.listener(Self::show_extensions))
            .on_action(cx.listener(Self::import_settings))
            .on_action(cx.listener(Self::debug_start))
            .on_action(cx.listener(Self::debug_pick))
            .on_action(cx.listener(Self::debug_stop))
            .on_action(cx.listener(Self::debug_over))
            .on_action(cx.listener(Self::debug_in))
            .on_action(cx.listener(Self::debug_out))
            .on_action(cx.listener(Self::show_inline_edit))
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
                        let agent = self.agent_shown;
                        let tab = |id: &'static str, label: &'static str, active: bool| {
                            div()
                                .id(id)
                                .debug_selector(move || id.into())
                                .h(px(24.))
                                .px_1p5()
                                .flex()
                                .items_center()
                                .rounded(px(8.))
                                .text_size(UI_FONT_SIZE)
                                .text_color(if active { theme.fg } else { theme.fg_subtle })
                                .when(active, |d| d.bg(theme.bg_elev))
                                .hover(|d| d.text_color(theme.fg))
                                .child(label)
                        };
                        d.child(
                            div()
                                .w(px(CHAT_WIDTH))
                                .flex_none()
                                .h_full()
                                .flex()
                                .flex_col()
                                .border_l_1()
                                .border_color(theme.line)
                                .child(
                                    div()
                                        .flex_none()
                                        .px_2()
                                        .py_1()
                                        .flex()
                                        .gap_1()
                                        .border_b_1()
                                        .border_color(theme.line)
                                        .child(tab("right-chat", "Chat", !agent).on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.show_right(false, window, cx)
                                            }),
                                        ))
                                        .child(tab("right-agent", "Agent", agent).on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.show_right(true, window, cx)
                                            }),
                                        )),
                                )
                                .child(div().flex_1().min_h_0().map(|d| {
                                    if agent {
                                        d.child(self.agent.clone())
                                    } else {
                                        d.child(self.chat.clone())
                                    }
                                })),
                        )
                    }),
            )
            .when(self.dock_open && self.dock_has_tabs(), |d| {
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

    /// Where an element is, once it is on screen.
    fn bounds_soon(
        cx: &mut VisualTestContext,
        selector: &'static str,
    ) -> gpui::Bounds<gpui::Pixels> {
        for _ in 0..500 {
            cx.executor().advance_clock(Duration::from_millis(50));
            cx.run_until_parked();
            if let Some(bounds) = cx.debug_bounds(selector) {
                return bounds;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {selector} on screen");
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
            ws.dock_view = DockView::Results;
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
            assert!(ws.dock_open && ws.show_results && ws.dock_view == DockView::Results);
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
        assert!(cx.read(|cx| ws.read(cx).dock_view == DockView::Response && ws.read(cx).dock_open));

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
        fake_api_reply(None, Duration::ZERO, true)
    }

    fn fake_api_reply(
        reply: Option<&'static str>,
        pause: Duration,
        complete: bool,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
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
                let answer = reply
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Echo: {question}"));
                let split = answer
                    .char_indices()
                    .nth(8)
                    .map_or(answer.len(), |(index, _)| index);
                for (index, chunk) in [&answer[..split], &answer[split..]].into_iter().enumerate() {
                    let data = if anthropic {
                        serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text": chunk}})
                    } else {
                        serde_json::json!({"choices":[{"delta":{"content": chunk}}]})
                    };
                    let _ = write!(stream, "data: {data}\n\n");
                    let _ = stream.flush();
                    if index == 0 {
                        std::thread::sleep(pause);
                    }
                }
                if complete {
                    let end = if anthropic {
                        "data: {\"type\":\"message_stop\"}\n\n"
                    } else {
                        "data: [DONE]\n\n"
                    };
                    let _ = write!(stream, "{end}");
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

    fn inline_panel(ws: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<InlineEdit> {
        cx.simulate_keystrokes("secondary-i");
        cx.read(|cx| {
            ws.read(cx)
                .inline_edit
                .as_ref()
                .expect("inline panel")
                .0
                .clone()
        })
    }

    fn choose_edit_model(store: &Entity<crate::ai_store::AiStore>, cx: &mut VisualTestContext) {
        wait_for(cx, "the edit provider", &|cx| {
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
            )
        });
    }

    #[gpui::test]
    fn inline_edit_reviews_selection_and_applies_with_one_undo_in_split_views(
        cx: &mut TestAppContext,
    ) {
        let (api, seen) = fake_api_reply(Some("```rs\nrenamed();\n```"), Duration::ZERO, true);
        let (root, store, ws, cx) = ai_setup(
            cx,
            "inline-selection",
            crate::ai_providers::Urls {
                lm_studio: api,
                ..down_urls()
            },
        );
        let path = root.join("src/main.rs");
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(path.clone(), None, window, cx)
        });
        cx.run_until_parked();
        let first = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let original = cx.read(|cx| first.read(cx).text(cx));
        cx.simulate_keystrokes("secondary-\\");
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let start = original.find("helper();").unwrap();
        let range = start..start + "helper();".len();
        editor.update(cx, |editor, cx| editor.select_range(range.clone(), cx));
        let panel = inline_panel(&ws, cx);
        choose_edit_model(&store, cx);
        cx.simulate_input("Rename the call");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the edit preview", &|cx| panel.read(cx).ready());
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), original);
        let request = seen
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.starts_with("POST"))
            .unwrap()
            .clone();
        let body: serde_json::Value =
            serde_json::from_str(request.split_once('\n').unwrap().1).unwrap();
        let context: serde_json::Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(context["target"], "helper();");
        assert!(context["before"].as_str().unwrap().contains("fn main"));
        assert!(context["project_map"].is_null());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("edit-apply").is_some());
        first.update(cx, |editor, cx| {
            editor.select_range(original.len()..original.len(), cx)
        });
        editor.update(cx, |editor, cx| {
            editor.select_range(start + 1..start + 1, cx)
        });
        cx.simulate_keystrokes("secondary-enter");
        wait_for(cx, "the edit applied", &|cx| {
            ws.read(cx).inline_edit.is_none()
        });
        let expected = original.replace("helper();", "renamed();");
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), expected);
        assert_eq!(cx.read(|cx| first.read(cx).text(cx)), expected);
        assert_eq!(
            cx.read(|cx| first.read(cx).newest_range()),
            expected.len()..expected.len()
        );
        assert_eq!(
            cx.read(|cx| editor.read(cx).newest_range()),
            start + "renamed();".len()..start + "renamed();".len()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        cx.simulate_keystrokes("secondary-z");
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), original);
        assert_eq!(
            cx.read(|cx| editor.read(cx).newest_range()),
            start + 1..start + 1
        );
        cx.simulate_keystrokes("secondary-shift-z");
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), expected);
    }

    #[gpui::test]
    fn inline_edit_whole_file_can_delete_and_rechecks_privacy_before_apply(
        cx: &mut TestAppContext,
    ) {
        let (api, _) = fake_api_reply(Some("```\n```"), Duration::ZERO, true);
        let (root, store, ws, cx) = ai_setup(
            cx,
            "inline-delete",
            crate::ai_providers::Urls {
                lm_studio: api,
                ..down_urls()
            },
        );
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("src/main.rs"), None, window, cx)
        });
        cx.run_until_parked();
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let original = cx.read(|cx| editor.read(cx).text(cx));
        let panel = inline_panel(&ws, cx);
        choose_edit_model(&store, cx);
        cx.simulate_input("Delete file contents");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "deletion preview", &|cx| panel.read(cx).ready());
        std::fs::write(root.join(".solderignore"), "src/main.rs\n").unwrap();
        cx.simulate_keystrokes("secondary-enter");
        wait_for(cx, "privacy recheck", &|cx| {
            panel.read(cx).error().is_some()
        });
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), original);
        std::fs::write(root.join(".solderignore"), "").unwrap();
        cx.simulate_keystrokes("enter");
        wait_for(cx, "fresh deletion preview", &|cx| panel.read(cx).ready());
        cx.simulate_keystrokes("secondary-enter");
        wait_for(cx, "deletion applied", &|cx| {
            ws.read(cx).inline_edit.is_none()
        });
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), "");
        cx.simulate_keystrokes("secondary-z");
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), original);
    }

    #[gpui::test]
    fn inline_edit_cancels_partial_stream_and_invalidates_changed_files(cx: &mut TestAppContext) {
        let (api, _) = fake_api_reply(Some("```rs\nnew();\n```"), Duration::from_millis(250), true);
        let (root, store, ws, cx) = ai_setup(
            cx,
            "inline-stop",
            crate::ai_providers::Urls {
                lm_studio: api,
                ..down_urls()
            },
        );
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("src/main.rs"), None, window, cx)
        });
        cx.run_until_parked();
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let original = cx.read(|cx| editor.read(cx).text(cx));
        let panel = inline_panel(&ws, cx);
        choose_edit_model(&store, cx);
        cx.simulate_input("Rewrite");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "partial preview", &|cx| {
            panel.read(cx).has_streamed_text()
        });
        assert!(!cx.read(|cx| panel.read(cx).ready()));
        cx.simulate_keystrokes("escape secondary-enter");
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), original);
        cx.simulate_keystrokes("enter");
        wait_for(cx, "regenerated preview", &|cx| panel.read(cx).ready());
        editor.update(cx, |editor, cx| {
            editor.replace_ranges(vec![(0..0, "// changed\n".into())], cx)
        });
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| panel.read(cx).error().map(str::to_string)),
            Some("File changed. Generate again.".to_string())
        );
        cx.simulate_keystrokes("secondary-enter");
        assert_eq!(
            cx.read(|cx| editor.read(cx).text(cx)),
            format!("// changed\n{original}")
        );
        cx.simulate_keystrokes("enter");
        wait_for(cx, "stream after modification", &|cx| {
            panel.read(cx).has_streamed_text()
        });
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("src/util/strings.rs"), None, window, cx)
        });
        wait_for(cx, "tab switch closes the edit", &|cx| {
            ws.read(cx).inline_edit.is_none()
        });
        assert!(!cx.read(|cx| panel.read(cx).streaming()));
        assert_eq!(
            cx.read(|cx| editor.read(cx).text(cx)),
            format!("// changed\n{original}")
        );
    }

    #[gpui::test]
    fn inline_edit_blocks_private_files_without_sending_the_selection(cx: &mut TestAppContext) {
        let (api, seen) = fake_api_reply(Some("```\nnew\n```"), Duration::ZERO, true);
        let (root, store, ws, cx) = ai_setup(
            cx,
            "inline-private",
            crate::ai_providers::Urls {
                lm_studio: api,
                ..down_urls()
            },
        );
        std::fs::write(root.join(".solderignore"), "private.rs\n").unwrap();
        for name in [".env", ".ENV.local", "private.rs"] {
            let path = root.join(name);
            std::fs::write(&path, "secret-inline-marker").unwrap();
            ws.update_in(cx, |workspace, window, cx| {
                workspace.open_path(path.clone(), None, window, cx)
            });
            wait_for(cx, "private tab", &|cx| {
                ws.read(cx)
                    .active_editor()
                    .is_some_and(|editor| editor.read(cx).path(cx) == Some(path.as_path()))
            });
            let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
            editor.update(cx, |editor, cx| editor.select_range(0..6, cx));
            let panel = inline_panel(&ws, cx);
            choose_edit_model(&store, cx);
            cx.simulate_input("Replace");
            cx.simulate_keystrokes("enter");
            wait_for(cx, "excluded edit", &|cx| panel.read(cx).error().is_some());
            assert!(!cx.read(|cx| panel.read(cx).ready()));
            cx.simulate_keystrokes("escape");
        }
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .all(|request| !request.starts_with("POST"))
        );
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .all(|request| !request.contains("secret-inline-marker"))
        );
    }

    #[gpui::test]
    fn inline_edit_rejects_a_disconnected_response_even_with_a_complete_block(
        cx: &mut TestAppContext,
    ) {
        let (api, _) = fake_api_reply(Some("```rs\nnew();\n```"), Duration::ZERO, false);
        let (root, store, ws, cx) = ai_setup(
            cx,
            "inline-disconnected",
            crate::ai_providers::Urls {
                lm_studio: api,
                ..down_urls()
            },
        );
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("src/main.rs"), None, window, cx)
        });
        cx.run_until_parked();
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let original = cx.read(|cx| editor.read(cx).text(cx));
        let panel = inline_panel(&ws, cx);
        choose_edit_model(&store, cx);
        cx.simulate_input("Rewrite");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "disconnected error", &|cx| {
            panel.read(cx).error().is_some()
        });
        assert!(cx.read(|cx| panel.read(cx).error().unwrap().contains("disconnected")));
        cx.simulate_keystrokes("secondary-enter");
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), original);
    }

    #[gpui::test]
    fn inline_edit_requires_a_model_and_a_small_writable_single_target(cx: &mut TestAppContext) {
        let (api, seen) = fake_api_reply(Some("```\nnew\n```"), Duration::ZERO, true);
        let (root, store, ws, cx) = ai_setup(
            cx,
            "inline-guards",
            crate::ai_providers::Urls {
                lm_studio: api,
                ..down_urls()
            },
        );
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("src/main.rs"), None, window, cx)
        });
        cx.run_until_parked();
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let panel = inline_panel(&ws, cx);
        cx.simulate_input("Rewrite");
        cx.simulate_keystrokes("enter");
        assert_eq!(
            cx.read(|cx| panel.read(cx).error().map(str::to_string)),
            Some("Choose a chat model first.".into())
        );
        choose_edit_model(&store, cx);
        editor.update(cx, |editor, cx| editor.select_ranges(&[0..0, 3..3], cx));
        cx.simulate_keystrokes("enter");
        assert!(cx.read(|cx| panel.read(cx).error().unwrap().contains("one selection")));
        cx.simulate_keystrokes("escape");
        for (path, content, message) in [
            (None, "untitled".to_string(), "Save this file"),
            (
                Some(root.join("src/main.rs")),
                "x".repeat(crate::chat_panel::FILE_LIMIT + 1),
                "smaller part",
            ),
            (
                Some(root.parent().unwrap().join("outside.rs")),
                "outside".to_string(),
                "inside this project",
            ),
        ] {
            ws.update_in(cx, |workspace, window, cx| {
                workspace.add_editor(path, &content, None, window, cx)
            });
            let panel = inline_panel(&ws, cx);
            cx.simulate_input("Rewrite");
            cx.simulate_keystrokes("enter");
            assert!(cx.read(|cx| panel.read(cx).error().unwrap().contains(message)));
            cx.simulate_keystrokes("escape");
        }
        let document = cx.new(|cx| {
            Document::virtual_file("Read only".into(), root.join("src/main.rs"), "code", cx)
        });
        let editor = cx.new(|cx| Editor::for_document(document, cx));
        ws.update_in(cx, |workspace, window, cx| {
            workspace.add_tab(editor, window, cx)
        });
        let panel = inline_panel(&ws, cx);
        cx.simulate_input("Rewrite");
        cx.simulate_keystrokes("enter");
        assert!(cx.read(|cx| panel.read(cx).error().unwrap().contains("read-only")));
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .all(|request| !request.starts_with("POST"))
        );
    }

    #[gpui::test]
    fn inline_edit_cancels_apply_when_the_target_tab_closes(cx: &mut TestAppContext) {
        let (api, _) = fake_api_reply(Some("```rs\nnew();\n```"), Duration::ZERO, true);
        let (root, store, ws, cx) = ai_setup(
            cx,
            "inline-close-apply",
            crate::ai_providers::Urls {
                lm_studio: api,
                ..down_urls()
            },
        );
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("src/main.rs"), None, window, cx)
        });
        cx.run_until_parked();
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let original = cx.read(|cx| editor.read(cx).text(cx));
        let panel = inline_panel(&ws, cx);
        choose_edit_model(&store, cx);
        cx.simulate_input("Rewrite");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the preview before closing", &|cx| {
            panel.read(cx).ready()
        });
        panel.update_in(cx, |panel, window, cx| {
            panel.apply(&crate::inline_edit::Apply, window, cx)
        });
        ws.update_in(cx, |workspace, window, cx| {
            workspace.remove_tab(&editor, window, cx)
        });
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).inline_edit.is_none()));
        assert!(!cx.read(|cx| panel.read(cx).streaming()));
        assert_eq!(cx.read(|cx| editor.read(cx).text(cx)), original);
    }

    #[gpui::test]
    fn inline_edit_map_is_opt_in_and_unchanged_proposals_are_not_applied(cx: &mut TestAppContext) {
        let (api, seen) = fake_api_reply(
            Some("```rs\nfn main() {\n    helper();\n}\n```"),
            Duration::ZERO,
            true,
        );
        let (root, store, ws, cx) = ai_setup(
            cx,
            "inline-map-noop",
            crate::ai_providers::Urls {
                lm_studio: api,
                ..down_urls()
            },
        );
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("src/main.rs"), None, window, cx)
        });
        cx.run_until_parked();
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let version = cx.read(|cx| editor.read(cx).version(cx));
        let panel = inline_panel(&ws, cx);
        choose_edit_model(&store, cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let toggle = cx.debug_bounds("edit-project").unwrap();
        cx.simulate_click(toggle.center(), gpui::Modifiers::default());
        cx.simulate_keystrokes("secondary-i");
        assert_eq!(
            cx.read(|cx| ws.read(cx).inline_edit.as_ref().unwrap().0.clone()),
            panel
        );
        cx.simulate_input("Keep the file as it is");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "unchanged preview", &|cx| {
            panel.read(cx).previewed() || panel.read(cx).error().is_some()
        });
        assert!(
            cx.read(|cx| panel.read(cx).previewed()),
            "{:?}",
            cx.read(|cx| panel.read(cx).error().map(str::to_string))
        );
        assert!(!cx.read(|cx| panel.read(cx).ready()));
        cx.simulate_keystrokes("secondary-enter");
        assert_eq!(cx.read(|cx| editor.read(cx).version(cx)), version);
        assert!(!cx.read(|cx| editor.read(cx).doc(cx).is_dirty()));
        let request = seen
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.starts_with("POST"))
            .unwrap()
            .clone();
        let body: serde_json::Value =
            serde_json::from_str(request.split_once('\n').unwrap().1).unwrap();
        let context: serde_json::Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert!(context["project_map"].as_str().unwrap().contains("helper"));
        cx.simulate_keystrokes("secondary-b");
        cx.simulate_resize(gpui::size(px(480.), px(320.)));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let bounds = cx.debug_bounds("inline-edit").unwrap();
        assert!(bounds.bottom() <= px(320. - STATUS_HEIGHT), "{bounds:?}");
        assert!(bounds.right() <= px(480.), "{bounds:?}");
        assert!(cx.debug_bounds("edit-apply").is_none());
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
        // The list is drawn from the providers' models, which may still be
        // settling when the picker opens: wait for the row, do not assume it.
        let model = bounds_soon(cx, "chat-model-custom-proxy-model-a");
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
    fn chat_project_map_is_opt_in_and_excludes_private_files(cx: &mut TestAppContext) {
        let (api, seen) = fake_api();
        let urls = crate::ai_providers::Urls {
            lm_studio: api,
            ..down_urls()
        };
        let (root, store, ws, cx) = ai_setup(cx, "chat-project-map", urls);
        std::fs::write(root.join("src/private.rs"), "fn PRIVATE_DECLARATION() {}\n").unwrap();
        std::fs::write(root.join(".env"), "ENV_SECRET=env-marker\n").unwrap();
        std::fs::write(root.join(".solderignore"), "src/private.rs\n!.env\n").unwrap();
        std::fs::write(
            root.join("src/util.rs"),
            "pub fn helper() { let value = \"BODY_SECRET\"; }\n",
        )
        .unwrap();
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("src/main.rs"), None, window, cx)
        });
        wait_for(cx, "the source file", &|cx| {
            ws.read(cx).active_editor().is_some_and(|editor| {
                editor.read(cx).path(cx) == Some(root.join("src/main.rs").as_path())
            })
        });
        cx.simulate_keystrokes("secondary-shift-l");
        wait_for(cx, "the map test provider", &|cx| {
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
            )
        });
        let chat = cx.read(|cx| ws.read(cx).chat.clone());
        let input = cx.read(|cx| chat.read(cx).input());
        assert!(!cx.read(|cx| chat.read(cx).include_project));
        cx.run_until_parked();
        let file_toggle = cx.debug_bounds("chat-file").unwrap();
        cx.simulate_click(file_toggle.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("Where is helper?");
        cx.simulate_keystrokes("enter");
        assert_eq!(chat_answer(&ws, cx), "Echo: Where is helper?");
        assert!(!seen.lock().unwrap().last().unwrap().contains("Project map"));
        let map_toggle = cx.debug_bounds("chat-project").unwrap();
        assert!(map_toggle.size.width > px(0.) && map_toggle.size.height > px(0.));
        cx.simulate_click(map_toggle.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("Find helper");
        cx.simulate_keystrokes("enter");
        assert_eq!(chat_answer(&ws, cx), "Echo: Find helper");
        let request = seen.lock().unwrap().last().unwrap().clone();
        assert!(request.contains("Project map") && request.contains("src/util.rs"));
        let body: serde_json::Value =
            serde_json::from_str(request.split_once('\n').unwrap().1).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert!(
            !body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("src/util.rs")
        );
        assert_eq!(
            body["messages"].as_array().unwrap().last().unwrap()["role"],
            "user"
        );
        assert!(request.contains(r#"fn \"helper\":1"#), "{request}");
        for private in [
            "private.rs",
            "PRIVATE_DECLARATION",
            "BODY_SECRET",
            "env-marker",
            "\\\".env\\\"",
        ] {
            assert!(!request.contains(private), "{private}: {request}");
        }
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("Explain the names");
        cx.simulate_keystrokes("enter");
        chat_answer(&ws, cx);
        let request = seen.lock().unwrap().last().unwrap().clone();
        assert_eq!(request.matches("Project map").count(), 1);
        assert_eq!(request.matches("src/util.rs").count(), 1);
        cx.read(|cx| {
            assert!(
                chat.read(cx).messages[2]
                    .context_paths
                    .contains(&root.join("src/util.rs"))
            );
            assert!(!chat.read(cx).messages[2].prompt.contains("Project map"));
        });
    }

    #[gpui::test]
    fn chat_rechecks_history_and_requires_a_fresh_chat_after_exclusion(cx: &mut TestAppContext) {
        let (api, seen) = fake_api();
        let urls = crate::ai_providers::Urls {
            lm_studio: api,
            ..down_urls()
        };
        let (root, store, ws, cx) = ai_setup(cx, "chat-context-history", urls);
        ws.update_in(cx, |workspace, window, cx| {
            workspace.open_path(root.join("src/main.rs"), None, window, cx)
        });
        wait_for(cx, "the history source", &|cx| {
            ws.read(cx).active_editor().is_some_and(|editor| {
                editor.read(cx).path(cx) == Some(root.join("src/main.rs").as_path())
            })
        });
        cx.simulate_keystrokes("secondary-shift-l");
        wait_for(cx, "the history provider", &|cx| {
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
            )
        });
        let chat = cx.read(|cx| ws.read(cx).chat.clone());
        let input = cx.read(|cx| chat.read(cx).input());
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("Explain main");
        cx.simulate_keystrokes("enter");
        chat_answer(&ws, cx);
        std::fs::write(root.join(".solderignore"), "src/main.rs\n").unwrap();
        cx.simulate_input("And the rest?");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the changed rules", &|cx| !chat.read(cx).streaming());
        cx.read(|cx| {
            assert_eq!(
                chat.read(cx)
                    .messages
                    .last()
                    .unwrap()
                    .error
                    .as_ref()
                    .map(|error| error.as_str()),
                Some("Context rules changed. Start a new chat.")
            )
        });
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            1
        );
        chat.update_in(cx, |chat, window, cx| {
            chat.new_chat(&crate::chat_panel::NewChat, window, cx)
        });
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        editor.update(cx, |editor, cx| editor.select_range(0..2, cx));
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("A clean question");
        cx.simulate_keystrokes("enter");
        assert_eq!(chat_answer(&ws, cx), "Echo: A clean question");
        cx.read(|cx| assert!(chat.read(cx).messages[0].context.is_none()));
        assert!(
            !seen
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .contains("From src/main.rs")
        );
        std::fs::write(root.join(".solderignore"), "[z-a]\n").unwrap();
        cx.simulate_input("Check invalid rules");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "invalid context rules", &|cx| {
            !chat.read(cx).streaming()
        });
        cx.read(|cx| {
            assert_eq!(
                chat.read(cx)
                    .messages
                    .last()
                    .unwrap()
                    .error
                    .as_ref()
                    .map(|error| error.as_str()),
                Some("Invalid pattern in .solderignore.")
            )
        });
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            2
        );
    }

    #[gpui::test]
    fn chat_stop_cancels_context_and_new_chat_has_no_stale_prompt(cx: &mut TestAppContext) {
        let (api, seen) = fake_api();
        let urls = crate::ai_providers::Urls {
            lm_studio: api,
            ..down_urls()
        };
        let (_root, store, ws, cx) = ai_setup(cx, "chat-context-stop", urls);
        cx.simulate_keystrokes("secondary-shift-l");
        wait_for(cx, "the stop test provider", &|cx| {
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
            )
        });
        let chat = cx.read(|cx| ws.read(cx).chat.clone());
        let input = cx.read(|cx| chat.read(cx).input());
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("Cancelled question");
        chat.update_in(cx, |chat, window, cx| {
            chat.include_project = true;
            chat.send(&crate::chat_panel::Send, window, cx);
            assert!(chat.streaming());
            chat.stop(&crate::chat_panel::Stop, window, cx);
            chat.new_chat(&crate::chat_panel::NewChat, window, cx);
        });
        cx.run_until_parked();
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .all(|request| !request.starts_with("POST "))
        );
        cx.simulate_input("Fresh question");
        cx.simulate_keystrokes("enter");
        assert_eq!(chat_answer(&ws, cx), "Echo: Fresh question");
        assert!(
            !seen
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .contains("Cancelled question")
        );
        assert_eq!(cx.read(|cx| chat.read(cx).messages.len()), 2);
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
            assert!(s.is_running(&model.id));
        });
        // Removing the model stops its server.
        let model = model.clone();
        store.update(cx, |s, cx| s.remove(model, cx));
        assert!(cx.read(|cx| store.read(cx).local.is_empty()));
    }

    /// The mock llama.cpp server installed, with the calibration model.
    fn mock_local_model(
        root: &Path,
        store: &Entity<crate::ai_store::AiStore>,
        cx: &mut VisualTestContext,
    ) -> ai::Model {
        let data = root.join("data");
        let bin = data.join("llama").join(ai::install::LLAMA_BUILD);
        std::fs::create_dir_all(&bin).unwrap();
        let mock =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../ai/tests/fixtures/mock_llama_server.py");
        std::fs::copy(mock, bin.join("llama-server")).unwrap();
        let model = ai::catalog::calibration().clone();
        let file = ai::Dirs::new(&data).model(&model);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "GGUF").unwrap();
        store.update(cx, |s, cx| s.load(cx));
        wait_for(cx, "the install", &|cx| {
            store.read(cx).installed.contains(&model.id)
        });
        model
    }

    fn ghost_text(ws: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<String> {
        cx.read(|cx| {
            let editor = ws.read(cx).active_editor()?.read(cx);
            editor
                .showing_ghost()
                .then(|| editor.ghost.as_ref().unwrap().text.clone())
        })
    }

    #[gpui::test]
    fn completions_suggest_stream_and_insert_at_the_cursor(cx: &mut TestAppContext) {
        let (root, store, ws, cx) = ai_setup(cx, "ghost", down_urls());
        let model = mock_local_model(&root, &store, cx);
        store.update(cx, |s, cx| s.set_role(ai::Role::Completion, &model.id, cx));
        let file = root.join("src/add.js");
        std::fs::write(&file, "function add(a, b) {\n  \n}\n").unwrap();
        ws.update_in(cx, |w, window, cx| {
            w.open_path(file.clone(), None, window, cx)
        });
        cx.run_until_parked();
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let at = "function add(a, b) {\n  ".len();
        editor.update(cx, |e, cx| {
            e.select_ranges(std::slice::from_ref(&(at..at)), cx)
        });

        // A pause after typing asks; the answer streams in at the cursor.
        cx.simulate_input(" ");
        wait_for(cx, "a suggestion", &|cx| {
            let e = ws.read(cx).active_editor().unwrap().read(cx);
            e.showing_ghost() && e.ghost_task.is_none()
        });
        assert_eq!(ghost_text(&ws, cx).as_deref(), Some("return a + b;"));
        assert!(cx.read(|cx| store.read(cx).is_running(&model.id)));

        // Typing what it suggests keeps the rest; the file has only what was typed.
        cx.simulate_input("ret");
        assert_eq!(ghost_text(&ws, cx).as_deref(), Some("urn a + b;"));
        assert_eq!(active_text(&ws, cx), "function add(a, b) {\n   ret\n}\n");

        // Tab inserts it, one step to undo.
        cx.simulate_keystrokes("tab");
        assert_eq!(
            active_text(&ws, cx),
            "function add(a, b) {\n   return a + b;\n}\n"
        );
        assert_eq!(ghost_text(&ws, cx), None);
        cx.simulate_keystrokes("secondary-z");
        assert_eq!(active_text(&ws, cx), "function add(a, b) {\n   ret\n}\n");

        // Escape dismisses; moving the cursor drops it too.
        cx.simulate_input(" ");
        wait_for(cx, "another suggestion", &|cx| {
            ws.read(cx)
                .active_editor()
                .unwrap()
                .read(cx)
                .showing_ghost()
        });
        cx.simulate_keystrokes("escape");
        assert_eq!(ghost_text(&ws, cx), None);
        assert_eq!(active_text(&ws, cx), "function add(a, b) {\n   ret \n}\n");
        cx.simulate_input("x");
        wait_for(cx, "a third suggestion", &|cx| {
            ws.read(cx)
                .active_editor()
                .unwrap()
                .read(cx)
                .showing_ghost()
        });
        cx.simulate_keystrokes("left");
        assert_eq!(ghost_text(&ws, cx), None);

        // Off: nothing is asked.
        store.update(cx, |s, cx| s.set_completions(false, cx));
        cx.simulate_keystrokes("end");
        cx.simulate_input("y");
        settle(cx);
        assert_eq!(ghost_text(&ws, cx), None);
        store.update(cx, |s, _| s.stop_local());
    }

    #[gpui::test]
    fn completions_respect_exclusions_and_fall_back_to_chat(cx: &mut TestAppContext) {
        let (root, store, ws, cx) = ai_setup(cx, "ghost-rules", down_urls());
        mock_local_model(&root, &store, cx);
        // A model without fill-in-the-middle tokens, added from disk.
        let own = root.join("models/own-nofim.gguf");
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, tiny_gguf()).unwrap();
        store.update(cx, |s, cx| s.add(own.display().to_string(), cx));
        wait_for(cx, "the added model", &|cx| {
            !store.read(cx).custom.is_empty()
        });
        let id = cx.read(|cx| store.read(cx).custom[0].id.clone());
        store.update(cx, |s, cx| s.set_role(ai::Role::Completion, &id, cx));

        std::fs::write(root.join(".solderignore"), "secret/\n").unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        for name in ["secret/key.js", "src/mul.js"] {
            std::fs::write(root.join(name), "function f(a, b) {\n  \n}\n").unwrap();
        }
        let at = "function f(a, b) {\n  ".len();
        let type_in = |name: &str, cx: &mut VisualTestContext| {
            ws.update_in(cx, |w, window, cx| {
                w.open_path(root.join(name), None, window, cx)
            });
            cx.run_until_parked();
            let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
            editor.update(cx, |e, cx| {
                e.select_ranges(std::slice::from_ref(&(at..at)), cx)
            });
            cx.simulate_input("r");
        };

        // A file .solderignore keeps from AI gets nothing, and nothing starts.
        type_in("secret/key.js", cx);
        settle(cx);
        assert_eq!(ghost_text(&ws, cx), None);
        assert!(cx.read(|cx| store.read(cx).local.is_empty()));

        // This model cannot fill in the middle: asked through chat instead,
        // with the fence the chat answer wraps it in removed.
        type_in("src/mul.js", cx);
        wait_for(cx, "a suggestion through chat", &|cx| {
            let e = ws.read(cx).active_editor().unwrap().read(cx);
            e.showing_ghost() && e.ghost_task.is_none()
        });
        assert_eq!(ghost_text(&ws, cx).as_deref(), Some("a * b;"));
        assert!(cx.read(|cx| store.read(cx).no_infill.contains(&id)));
        store.update(cx, |s, _| s.stop_local());
    }

    /// A model that answers each step from a script: one OpenAI-style
    /// response per request, in order. Records the requests.
    fn scripted_model(
        steps: Vec<serde_json::Value>,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            let mut steps = steps.into_iter();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap_or(0);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).ok();
                let json = if first.starts_with("GET") {
                    r#"{"data":[{"id":"agent-model"}]}"#.to_string()
                } else {
                    log.lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap_or_default());
                    steps.next().unwrap_or_else(|| serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":"(script ended)"}}]})).to_string()
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
            }
        });
        (base, seen)
    }

    fn tool_step(id: &str, name: &str, input: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"choices":[{"finish_reason":"tool_calls","message":{
            "content":"",
            "tool_calls":[{"id": id, "type":"function","function":{"name": name, "arguments": input.to_string()}}],
        }}]})
    }

    #[gpui::test]
    fn agent_plans_works_asks_and_merges_on_its_own_branch(cx: &mut TestAppContext) {
        use crate::agent_task::Status;
        let (api, seen) = scripted_model(vec![
            tool_step(
                "p1",
                "update_plan",
                serde_json::json!({"steps": [{"text": "Fix add"}, {"text": "Check it"}]}),
            ),
            tool_step(
                "e1",
                "edit_file",
                serde_json::json!({"path": "src/lib.rs", "old": "a - b", "new": "a + b"}),
            ),
            tool_step(
                "r1",
                "run",
                serde_json::json!({"command": "grep -q 'a + b' src/lib.rs && echo checked", "access": "full", "reason": "to check"}),
            ),
            tool_step(
                "f1",
                "finish",
                serde_json::json!({"summary": "Fixed add and checked it."}),
            ),
        ]);
        // A repository with one commit, and AI data kept outside it.
        let repo = db::testing::dir("agent-flow");
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let repo = repo.canonicalize().unwrap();
        let data = db::testing::dir("agent-flow-data");
        cx.executor().allow_parking();
        let store = cx.update(|cx| {
            let store = cx.new(|_| {
                crate::ai_store::AiStore::new(ai::Dirs::new(&data), "http://127.0.0.1:9".into())
                    .with_scan_dirs(Vec::new())
                    .for_tests(down_urls())
            });
            crate::ai_store::AiStore::set_global(store.clone(), cx);
            store
        });
        let (ws, cx) = setup(cx, repo.clone());
        store.update(cx, |s, cx| {
            s.load(cx);
            s.add_provider("Mock".into(), api.clone(), None, cx);
        });
        wait_for(cx, "the provider", &|cx| {
            store
                .read(cx)
                .provider("custom-mock")
                .is_some_and(|p| !p.models().is_empty())
        });
        store.update(cx, |s, cx| {
            s.choose(
                ai::Role::Chat,
                crate::ai_providers::ModelRef {
                    provider: "custom-mock".into(),
                    model: "agent-model".into(),
                },
                cx,
            )
        });

        cx.simulate_keystrokes("secondary-shift-i");
        cx.simulate_input("Fix the add function");
        cx.simulate_keystrokes("enter");
        let panel = cx.read(|cx| ws.read(cx).agent.clone());
        let status = |cx: &mut VisualTestContext| {
            cx.read(|cx| {
                panel
                    .read(cx)
                    .task
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .status
                    .clone()
            })
        };
        wait_for(cx, "the plan", &|cx| {
            panel.read(cx).task.as_ref().unwrap().read(cx).status == Status::AwaitingPlan
        });
        let task = cx.read(|cx| panel.read(cx).task.clone().unwrap());
        let worktree = cx.read(|cx| task.read(cx).worktree.clone().unwrap());
        assert_eq!(worktree.branch, "solder/agent/fix-the-add-function");
        // Planning offered no tools that change anything.
        let first = seen.lock().unwrap()[0].clone();
        let tools: Vec<String> = first["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert!(
            !tools.contains(&"edit_file".to_string()) && tools.contains(&"update_plan".to_string()),
            "{tools:?}"
        );

        // Approving starts the work; the command that wants more than the
        // sandbox waits.
        cx.run_until_parked();
        let approve = cx.debug_bounds("agent-approve").expect("approve");
        cx.simulate_click(approve.center(), gpui::Modifiers::default());
        wait_for(cx, "the request to run", &|cx| {
            matches!(
                panel.read(cx).task.as_ref().unwrap().read(cx).status,
                Status::AwaitingApproval { .. }
            )
        });
        assert!(
            std::fs::read_to_string(worktree.path.join("src/lib.rs"))
                .unwrap()
                .contains("a + b")
        );
        // The user's checkout is untouched.
        assert!(
            std::fs::read_to_string(repo.join("src/lib.rs"))
                .unwrap()
                .contains("a - b")
        );
        cx.run_until_parked();
        let allow = cx.debug_bounds("agent-allow").expect("allow");
        cx.simulate_click(allow.center(), gpui::Modifiers::default());
        wait_for(cx, "the end", &|cx| {
            let t = panel.read(cx).task.as_ref().unwrap().read(cx);
            t.status == Status::Finished && !t.changes.is_empty()
        });
        cx.read(|cx| {
            let t = task.read(cx);
            assert_eq!(t.summary.as_deref(), Some("Fixed add and checked it."));
            assert!(t.plan.iter().all(|s| s.done));
            assert_eq!(t.changes[0].path, "src/lib.rs");
        });
        let requests = seen.lock().unwrap().clone();
        let approved = requests[1]["messages"].to_string();
        assert!(approved.contains("The plan is approved"), "{approved}");
        let after_run = requests[3]["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(after_run["role"], "tool");
        assert!(
            after_run["content"].as_str().unwrap().contains("checked"),
            "{after_run}"
        );

        // Review a change, then merge it into the user's branch.
        cx.run_until_parked();
        let change = cx.debug_bounds("agent-change-0").expect("a change");
        cx.simulate_click(change.center(), gpui::Modifiers::default());
        wait_for(cx, "the diff", &|cx| ws.read(cx).file_diff.is_some());
        let merge = cx.debug_bounds("agent-merge").expect("merge");
        cx.simulate_click(merge.center(), gpui::Modifiers::default());
        wait_for(cx, "the merge", &|cx| {
            panel.read(cx).task.as_ref().unwrap().read(cx).status == Status::Merged
        });
        assert!(
            std::fs::read_to_string(repo.join("src/lib.rs"))
                .unwrap()
                .contains("a + b")
        );
        assert!(!worktree.path.exists());
        assert_eq!(status(cx), Status::Merged);
    }

    #[gpui::test]
    fn agent_stops_and_takes_new_instructions(cx: &mut TestAppContext) {
        use crate::agent_task::Status;
        let (api, seen) = scripted_model(vec![
            tool_step(
                "p1",
                "update_plan",
                serde_json::json!({"steps": ["Try it"]}),
            ),
            tool_step(
                "r1",
                "run",
                serde_json::json!({"command": "curl https://example.com", "access": "network", "reason": "download"}),
            ),
            tool_step(
                "f1",
                "finish",
                serde_json::json!({"summary": "Did it without the network."}),
            ),
        ]);
        let repo = db::testing::dir("agent-deny");
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let repo = repo.canonicalize().unwrap();
        let data = db::testing::dir("agent-deny-data");
        cx.executor().allow_parking();
        let store = cx.update(|cx| {
            let store = cx.new(|_| {
                crate::ai_store::AiStore::new(ai::Dirs::new(&data), "http://127.0.0.1:9".into())
                    .with_scan_dirs(Vec::new())
                    .for_tests(down_urls())
            });
            crate::ai_store::AiStore::set_global(store.clone(), cx);
            store
        });
        let (ws, cx) = setup(cx, repo.clone());
        store.update(cx, |s, cx| {
            s.load(cx);
            s.add_provider("Mock".into(), api, None, cx);
        });
        wait_for(cx, "the provider", &|cx| {
            store
                .read(cx)
                .provider("custom-mock")
                .is_some_and(|p| !p.models().is_empty())
        });
        store.update(cx, |s, cx| {
            s.choose(
                ai::Role::Chat,
                crate::ai_providers::ModelRef {
                    provider: "custom-mock".into(),
                    model: "agent-model".into(),
                },
                cx,
            )
        });
        cx.simulate_keystrokes("secondary-shift-i");
        cx.simulate_input("Do the thing");
        cx.simulate_keystrokes("enter");
        let panel = cx.read(|cx| ws.read(cx).agent.clone());
        wait_for(cx, "the plan", &|cx| {
            panel.read(cx).task.as_ref().unwrap().read(cx).status == Status::AwaitingPlan
        });
        let task = cx.read(|cx| panel.read(cx).task.clone().unwrap());
        task.update(cx, |t, cx| {
            t.approve_plan(vec!["Try it".into(), "  ".into()], cx)
        });
        wait_for(cx, "the network request", &|cx| {
            matches!(task.read(cx).status, Status::AwaitingApproval { .. })
        });
        // A new instruction declines the waiting command and goes on.
        let input = cx.read(|cx| panel.read(cx).input());
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("Skip the download");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the end", &|cx| {
            task.read(cx).status == Status::Finished
        });
        let last = seen.lock().unwrap().last().unwrap()["messages"].clone();
        let messages = last.as_array().unwrap();
        let declined = messages
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == "r1")
            .unwrap();
        assert!(
            declined["content"].as_str().unwrap().contains("Not run"),
            "{declined}"
        );
        assert_eq!(messages.last().unwrap()["content"], "Skip the download");
        cx.read(|cx| assert!(task.read(cx).changes.is_empty()));

        // Discard removes the worktree and its branch.
        let wt = cx.read(|cx| task.read(cx).worktree.clone().unwrap());
        task.update(cx, |t, cx| t.discard(cx));
        wait_for(cx, "the worktree to go", &|_| !wt.path.exists());
        let branches = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["branch", "--list", "solder/*"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&branches.stdout).trim().is_empty());
    }

    fn review_repo(name: &str) -> PathBuf {
        let repo = db::testing::dir(name);
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(repo.join("a.rs"), "fn a() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        git(&["switch", "-q", "-c", "feature"]);
        std::fs::write(repo.join("a.rs"), "fn a() {\n    let x = 1 / 0;\n}\n").unwrap();
        git(&["commit", "-qam", "divide"]);
        repo.canonicalize().unwrap()
    }

    /// Requests the scripted model received.
    type Seen = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

    fn review_setup<'a>(
        cx: &'a mut TestAppContext,
        name: &str,
        model: Vec<serde_json::Value>,
    ) -> (
        PathBuf,
        Entity<crate::ai_store::AiStore>,
        Entity<Workspace>,
        Seen,
        &'a mut VisualTestContext,
    ) {
        let (api, seen) = scripted_model(model);
        let repo = review_repo(name);
        let data = db::testing::dir(&format!("{name}-data"));
        cx.executor().allow_parking();
        let store = cx.update(|cx| {
            let store = cx.new(|_| {
                crate::ai_store::AiStore::new(ai::Dirs::new(&data), "http://127.0.0.1:9".into())
                    .with_scan_dirs(Vec::new())
                    .for_tests(down_urls())
            });
            crate::ai_store::AiStore::set_global(store.clone(), cx);
            store
        });
        let (ws, cx) = setup(cx, repo.clone());
        store.update(cx, |s, cx| {
            s.load(cx);
            s.add_provider("Mock".into(), api, None, cx);
        });
        wait_for(cx, "the provider", &|cx| {
            store
                .read(cx)
                .provider("custom-mock")
                .is_some_and(|p| !p.models().is_empty())
        });
        store.update(cx, |s, cx| {
            s.choose(
                ai::Role::Chat,
                crate::ai_providers::ModelRef {
                    provider: "custom-mock".into(),
                    model: "agent-model".into(),
                },
                cx,
            )
        });
        (repo, store, ws, seen, cx)
    }

    #[gpui::test]
    fn push_waits_for_review_findings(cx: &mut TestAppContext) {
        let finding = tool_step(
            "r1",
            "report_findings",
            serde_json::json!({"findings": [
                {"file": "a.rs", "line": 2, "severity": "bug", "message": "Divides by zero."},
            ]}),
        );
        let (repo, _store, ws, seen, cx) = review_setup(cx, "review-found", vec![finding]);
        cx.simulate_keystrokes("ctrl-shift-g");
        wait_for(cx, "the git status", &|cx| {
            ws.read(cx).git.read(cx).status().branch.is_some()
        });
        let push = cx.debug_bounds("git-push").expect("push button");
        cx.simulate_click(push.center(), gpui::Modifiers::default());
        let panel = cx.read(|cx| ws.read(cx).git_panel.clone());
        wait_for(cx, "the review", &|cx| {
            matches!(
                panel.read(cx).review,
                Some(crate::git_panel::PushReview::Found { .. })
            )
        });
        // Nothing was pushed.
        assert_eq!(cx.read(|cx| ws.read(cx).pushes), 0);
        let request = seen.lock().unwrap()[0].clone();
        let asked = request["messages"][1]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            asked.contains("1 commit to push onto main") && asked.contains("1 / 0"),
            "{asked}"
        );
        assert_eq!(request["tools"][0]["function"]["name"], "report_findings");

        // A finding opens its file at its line.
        cx.run_until_parked();
        let finding = cx.debug_bounds("review-finding-0").expect("a finding");
        cx.simulate_click(finding.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(active_path(&ws, cx), Some(repo.join("a.rs")));
        let row = cx.read(|cx| {
            let e = ws.read(cx).active_editor().unwrap().read(cx);
            e.buf(cx).offset_to_point(e.newest_range().start).row
        });
        assert_eq!(row, 1);

        // Cancel drops the review and pushes nothing.
        cx.simulate_keystrokes("ctrl-shift-g");
        cx.run_until_parked();
        let cancel = cx.debug_bounds("review-cancel").expect("cancel");
        cx.simulate_click(cancel.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.read(|cx| panel.read(cx).review.is_none()));
        assert_eq!(cx.read(|cx| ws.read(cx).pushes), 0);
    }

    #[gpui::test]
    fn a_clean_review_pushes_and_review_can_be_off(cx: &mut TestAppContext) {
        let clean = tool_step("r1", "report_findings", serde_json::json!({"findings": []}));
        let (_repo, store, ws, seen, cx) = review_setup(cx, "review-clean", vec![clean]);
        cx.simulate_keystrokes("ctrl-shift-g");
        wait_for(cx, "the git status", &|cx| {
            ws.read(cx).git.read(cx).status().branch.is_some()
        });
        let push = cx.debug_bounds("git-push").expect("push button");
        cx.simulate_click(push.center(), gpui::Modifiers::default());
        // Nothing found: the push goes ahead in a terminal.
        wait_for(cx, "the push", &|cx| ws.read(cx).pushes == 1);
        assert_eq!(seen.lock().unwrap().len(), 1);
        let panel = cx.read(|cx| ws.read(cx).git_panel.clone());
        assert!(cx.read(|cx| panel.read(cx).review.is_none()));

        // With review off, Push does not ask the model. (The terminal has
        // the focus now; Push is the Git panel's action.)
        store.update(cx, |s, cx| s.set_review_push(false, cx));
        cx.update(|window, cx| window.focus(&panel.focus_handle(cx)));
        cx.dispatch_action(crate::git_panel::ReviewAndPush);
        wait_for(cx, "the second push", &|cx| ws.read(cx).pushes == 2);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    // ------------------------------------------------------------ debugger

    /// A project with `app.js` and the debugger on `mock_dap.py`.
    fn debug_setup<'a>(
        cx: &'a mut TestAppContext,
        name: &str,
    ) -> (
        PathBuf,
        Entity<crate::debug::DebugStore>,
        Entity<Workspace>,
        &'a mut VisualTestContext,
    ) {
        let root = db::testing::dir(&format!("ws-{name}"))
            .canonicalize()
            .unwrap();
        std::fs::write(
            root.join("app.js"),
            "function add(a, b) {\n  const sum = a + b;\n  return sum;\n}\nconsole.log(add(2, 3));\n",
        )
        .unwrap();
        cx.executor().allow_parking();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_dap.py");
        let data = root.join("data");
        let store = cx.update(|cx| {
            let store = cx.new(|_| {
                crate::debug::DebugStore::new(
                    data,
                    crate::debug::AdapterSpec::Command {
                        program: "python3".into(),
                        args: vec![fixture.display().to_string()],
                    },
                )
            });
            crate::debug::DebugStore::set_global(store.clone(), cx);
            store
        });
        let (ws, cx) = setup(cx, root.clone());
        let app = root.join("app.js");
        ws.update_in(cx, |w, window, cx| w.open_path(app, None, window, cx));
        wait_for(cx, "app.js", &|cx| {
            ws.read(cx)
                .active_editor()
                .is_some_and(|e| e.read(cx).layout.is_some())
        });
        (root, store, ws, cx)
    }

    fn paused_line(store: &Entity<crate::debug::DebugStore>, cx: &App) -> Option<u32> {
        store.read(cx).paused.as_ref()?.frame().map(|f| f.line)
    }

    #[gpui::test]
    fn debugger_stops_steps_evaluates_and_records_queries(cx: &mut TestAppContext) {
        let (root, store, ws, cx) = debug_setup(cx, "debug-flow");
        let app = root.join("app.js");
        // A click on line 3's number sets a breakpoint and a second clears
        // it; F9 sets one on the cursor's line.
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let (left, top, line_height) = cx.read(|cx| {
            let layout = editor.read(cx).layout.as_ref().unwrap();
            (
                layout.bounds.left(),
                layout.bounds.top(),
                layout.line_height,
            )
        });
        let line_3 = gpui::point(left + px(4.), top + line_height * 2.5);
        cx.simulate_click(line_3, gpui::Modifiers::default());
        let lines = |cx: &mut VisualTestContext| {
            cx.read(|cx| {
                store
                    .read(cx)
                    .lines(&app)
                    .map(|l| l.keys().copied().collect::<Vec<_>>())
                    .unwrap_or_default()
            })
        };
        assert_eq!(lines(cx), [3]);
        cx.simulate_click(line_3, gpui::Modifiers::default());
        assert!(lines(cx).is_empty());
        cx.simulate_keystrokes("down f9");
        assert_eq!(lines(cx), [2]);

        // F5 runs the open file and stops on the breakpoint, verified.
        cx.simulate_keystrokes("f5");
        wait_for(cx, "the pause", &|cx| paused_line(&store, cx) == Some(2));
        assert_eq!(
            cx.read(|cx| store.read(cx).lines(&app).cloned()),
            Some([(2, true)].into())
        );
        assert_eq!(cx.read(|cx| ws.read(cx).dock_view), DockView::Debug);
        assert_eq!(active_path(&ws, cx), Some(app.clone()));

        // Variables, and an object opened one level.
        wait_for(cx, "the variables", &|cx| {
            store
                .read(cx)
                .children
                .get(&10)
                .is_some_and(|v| v.len() == 2)
        });
        let user = cx.debug_bounds("debug-var-2").expect("the user row");
        cx.simulate_click(user.center(), gpui::Modifiers::default());
        wait_for(cx, "the user's fields", &|cx| {
            store
                .read(cx)
                .children
                .get(&11)
                .is_some_and(|v| v[0].name == "name")
        });

        // The console runs an expression in the paused frame.
        let input = cx.read(|cx| ws.read(cx).debug_panel.read(cx).input());
        cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
        cx.simulate_input("a + 1");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the answer", &|cx| {
            store
                .read(cx)
                .console
                .iter()
                .any(|l| l.text == "seen a + 1")
        });

        // F10 steps a line, F5 runs to the end.
        cx.simulate_keystrokes("f10");
        wait_for(cx, "the step", &|cx| paused_line(&store, cx) == Some(3));
        cx.simulate_keystrokes("f5");
        wait_for(cx, "the end", &|cx| !store.read(cx).state.active());
        assert!(cx.read(|cx| store.read(cx).console.iter().any(|l| l.text == "done")));

        // The query the program ran, and a click on it opens where.
        wait_for(cx, "the timeline", &|cx| store.read(cx).timeline.len() == 1);
        let tab = cx
            .debug_bounds("debug-timeline-tab")
            .expect("the timeline tab");
        cx.simulate_click(tab.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let row = cx.debug_bounds("debug-timeline-0").expect("the query row");
        editor.update_in(cx, |e, _, cx| e.select_range(0..0, cx));
        cx.simulate_click(row.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| editor.read(cx).cursor_position(cx).0), 2);
    }

    #[gpui::test]
    fn a_server_run_opens_its_browser_and_stop_ends_both(cx: &mut TestAppContext) {
        let (root, store, _ws, cx) = debug_setup(cx, "debug-browser");
        let app = root.join("app.js");
        store.update(cx, |s, cx| s.toggle(&app, 2, cx));
        let config = crate::debug_launch::LaunchConfig {
            name: "npm run dev + browser".into(),
            request: serde_json::json!({
                "type": "pwa-node",
                "request": "launch",
                "program": app,
                "env": {},
            }),
            browser: Some("http://localhost:4123".into()),
        };
        store.update(cx, |s, cx| s.start(config, root.clone(), cx));
        // The server says where it listens; the page opens in a browser
        // session of the same run, named for what it is.
        wait_for(cx, "the browser session", &|cx| {
            let sessions = &store.read(cx).sessions;
            sessions.iter().filter(|s| s.name == "Browser").count() == 2
        });
        wait_for(cx, "the server's pause", &|cx| {
            paused_line(&store, cx) == Some(2)
        });
        cx.simulate_keystrokes("shift-f5");
        cx.run_until_parked();
        let s = cx.read(|cx| store.read(cx).sessions.len());
        assert_eq!(s, 0);
        assert!(cx.read(|cx| !store.read(cx).state.active()));
        assert!(cx.read(|cx| store.read(cx).paused.is_none()));
    }

    // -------------------------------------------------------------- plugins

    use crate::plugin_store::{PluginState, PluginStore};

    /// A project with `notes.txt` open and a plugins folder holding what
    /// `install` puts there; nothing is enabled.
    fn plugin_setup<'a>(
        cx: &'a mut TestAppContext,
        name: &str,
        install: impl FnOnce(&Path),
    ) -> (
        PathBuf,
        PathBuf,
        Entity<PluginStore>,
        Entity<Workspace>,
        &'a mut VisualTestContext,
    ) {
        let root = db::testing::dir(&format!("ws-{name}"))
            .canonicalize()
            .unwrap();
        std::fs::write(root.join("notes.txt"), "one two three").unwrap();
        let data = db::testing::dir(&format!("ws-{name}-data"));
        std::fs::create_dir_all(data.join("plugins")).unwrap();
        install(&data.join("plugins"));
        cx.executor().allow_parking();
        let store = cx.update(|cx| {
            let store = cx.new(|cx| PluginStore::new(data.clone(), cx));
            PluginStore::set_global(store.clone(), cx);
            store.update(cx, |s, cx| s.scan(cx));
            store
        });
        let (ws, cx) = setup(cx, root.clone());
        let notes = root.join("notes.txt");
        ws.update_in(cx, |w, window, cx| w.open_path(notes, None, window, cx));
        wait_for(cx, "the plugins folder", &|cx| {
            store.read(cx).loaded && ws.read(cx).active_editor().is_some()
        });
        (root, data, store, ws, cx)
    }

    fn plugin_status(store: &Entity<PluginStore>, name: &str, cx: &App) -> Option<String> {
        store.read(cx).status.get(name).map(|s| s.to_string())
    }

    #[gpui::test]
    fn a_plugin_runs_once_its_permissions_are_approved(cx: &mut TestAppContext) {
        let (_root, data, store, ws, cx) = plugin_setup(cx, "plugin-flow", |plugins| {
            plugin::testing::install(&plugin::testing::word_count(), plugins);
        });
        // Installed is not running: no status, no commands.
        assert_eq!(
            cx.read(|cx| store.read(cx).state("word-count")),
            PluginState::Disabled
        );
        assert!(cx.read(|cx| store.read(cx).commands().is_empty()));

        // The Plugins window lists it with what enabling allows.
        cx.dispatch_action(ShowPlugins);
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
        let enable = cx
            .debug_bounds("plugin-toggle-0")
            .expect("the Enable button");
        cx.simulate_click(enable.center(), gpui::Modifiers::default());
        assert_eq!(
            cx.read(|cx| store.read(cx).state("word-count")),
            PluginState::Enabled
        );
        // It hears about the file in front and shows its count.
        wait_for(cx, "the count", &|cx| {
            plugin_status(&store, "word-count", cx).as_deref() == Some("3 words")
        });
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).modal.is_none()));

        // Typing reaches it as changes.
        // The cursor to the end, without a key that differs by platform.
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        editor.update_in(cx, |e, _, cx| {
            let end = e.text(cx).len();
            e.select_range(end..end, cx)
        });
        cx.simulate_input(" four five");
        wait_for(cx, "the new count", &|cx| {
            plugin_status(&store, "word-count", cx).as_deref() == Some("5 words")
        });

        // Its command is in the palette and edits the file.
        cx.simulate_keystrokes("secondary-shift-p");
        cx.simulate_input("insert the count");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the insertion", &|cx| {
            ws.read(cx)
                .active_editor()
                .is_some_and(|e| e.read(cx).text(cx) == "one two three four five5 words")
        });

        // What was approved is kept for the next start.
        let saved = data.join("plugins.json");
        wait_for(cx, "plugins.json", &|_| {
            std::fs::read_to_string(&saved).is_ok_and(|t| t.contains("editor:write"))
        });

        // Off: its text and commands go.
        store.update(cx, |s, cx| s.disable("word-count", cx));
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| plugin_status(&store, "word-count", cx)), None);
        assert!(cx.read(|cx| store.read(cx).commands().is_empty()));
    }

    #[gpui::test]
    fn a_plugin_that_changed_stays_off_until_enabled_again(cx: &mut TestAppContext) {
        let (_root, data, store, _ws, cx) = plugin_setup(cx, "plugin-changed", |plugins| {
            plugin::testing::install(&plugin::testing::word_count(), plugins);
        });
        store.update(cx, |s, cx| s.enable("word-count", cx));
        wait_for(cx, "the count", &|cx| {
            plugin_status(&store, "word-count", cx).is_some()
        });

        // An update asks for more: it stops, and says so.
        let manifest = data.join("plugins/word-count/plugin.json");
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(
            &manifest,
            text.replace("\"statusBar\"", "\"statusBar\", \"fs:read\""),
        )
        .unwrap();
        store.update(cx, |s, cx| s.scan(cx));
        wait_for(cx, "the change", &|cx| {
            store.read(cx).state("word-count") == PluginState::Changed
        });
        assert_eq!(cx.read(|cx| plugin_status(&store, "word-count", cx)), None);
        assert!(cx.read(|cx| store.read(cx).stats("word-count").is_none()));

        // The same holds for another module under the old manifest, and
        // after a restart.
        std::fs::write(&manifest, text).unwrap();
        std::fs::write(
            data.join("plugins/word-count/plugin.wasm"),
            plugin::testing::build(&plugin::testing::probe()),
        )
        .unwrap();
        let restarted = cx.update(|_, cx| {
            let store = cx.new(|cx| PluginStore::new(data.clone(), cx));
            store.update(cx, |s, cx| s.scan(cx));
            store
        });
        wait_for(cx, "the restart", &|cx| restarted.read(cx).loaded);
        assert_eq!(
            cx.read(|cx| restarted.read(cx).state("word-count")),
            PluginState::Changed
        );
        assert!(cx.read(|cx| restarted.read(cx).stats("word-count").is_none()));

        // Enabling approves what is there now.
        restarted.update(cx, |s, cx| s.enable("word-count", cx));
        wait_for(cx, "the plugin", &|cx| {
            restarted.read(cx).stats("word-count").is_some()
        });
        assert_eq!(
            cx.read(|cx| restarted.read(cx).state("word-count")),
            PluginState::Enabled
        );
    }

    #[gpui::test]
    fn plugins_reach_the_files_and_hosts_they_declared(cx: &mut TestAppContext) {
        // A server that redirects once, then answers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for reply in [
                "HTTP/1.1 302 Found\r\nLocation: http://example.com/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = stream.read(&mut [0; 4096]);
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        let get = format!("get:http://127.0.0.1:{port}/x");
        let commands = [
            get.as_str(),
            "get:http://example.com/",
            "read:notes.txt",
            "read:.env",
            "read:../outside.txt",
        ];
        let (root, _data, store, _ws, cx) = plugin_setup(cx, "plugin-reach", |plugins| {
            let dir = plugin::testing::install(&plugin::testing::probe(), plugins);
            std::fs::write(
                dir.join("plugin.json"),
                serde_json::json!({
                    "name": "probe",
                    "permissions": ["statusBar", "fs:read", "http:127.0.0.1"],
                    "commands": commands.iter().map(|c| serde_json::json!({"id": c, "title": c})).collect::<Vec<_>>(),
                })
                .to_string(),
            )
            .unwrap();
        });
        std::fs::write(root.join(".env"), "SECRET=1").unwrap();
        store.update(cx, |s, cx| s.enable("probe", cx));
        wait_for(cx, "the probe", &|cx| {
            store.read(cx).stats("probe").is_some()
        });
        let mut run = |id: &str| {
            store.update(cx, |s, cx| {
                s.status.clear();
                s.run_command("probe", id);
                cx.notify();
            });
            wait_for(cx, id, &|cx| plugin_status(&store, "probe", cx).is_some());
            cx.read(|cx| plugin_status(&store, "probe", cx).unwrap())
        };
        assert_eq!(run("read:notes.txt"), "one two three");
        assert!(run("read:.env").contains("kept private"));
        assert!(run("read:../outside.txt").contains("without .."));
        // A redirect is handed back, not followed to another host.
        assert_eq!(run(&get), "302 ");
        assert_eq!(run(&get), "200 hello");
        assert!(
            run("get:http://example.com/")
                .contains("did not declare the permission http:example.com")
        );
    }

    #[gpui::test]
    fn a_plugin_that_holds_up_typing_is_marked_slow(cx: &mut TestAppContext) {
        let (_root, _data, store, _ws, cx) = plugin_setup(cx, "plugin-slow", |plugins| {
            let dir = plugin::testing::install(&plugin::testing::probe(), plugins);
            std::fs::write(
                dir.join("plugin.json"),
                r#"{"name": "probe", "permissions": ["statusBar"], "events": ["change"]}"#,
            )
            .unwrap();
        });
        store.update(cx, |s, cx| {
            // The probe's change handler works for milliseconds.
            s.set_budget(plugin::Budget {
                typing: Duration::from_micros(200),
                idle: Duration::from_millis(20),
                ..Default::default()
            });
            s.enable("probe", cx);
        });
        wait_for(cx, "the probe", &|cx| {
            store.read(cx).stats("probe").is_some()
        });
        assert!(cx.read(|cx| store.read(cx).slow().is_empty()));
        // Each edit is handled, late, and counted against the plugin.
        for round in 1..=plugin::SLOW_AFTER {
            cx.simulate_input("x");
            wait_for(cx, "the change", &|cx| {
                let stats = store.read(cx).stats("probe").unwrap();
                stats.events == u64::from(round) && stats.over_budget == round
            });
        }
        assert!(cx.read(|cx| store.read(cx).slow() == ["probe"]));
        assert_eq!(
            cx.read(|cx| plugin_status(&store, "probe", cx)).as_deref(),
            Some("change notes.txt")
        );
    }

    // --------------------------------------------------------------- import

    /// A home folder where VS Code keeps settings, a key binding, a theme
    /// from an extension and two other extensions.
    fn vscode_home(name: &str) -> import::Roots {
        let home = db::testing::dir(&format!("ws-{name}-home"));
        for (path, text) in [
            (
                "Library/Application Support/Code/User/settings.json",
                r#"{
                  // What comes over.
                  "editor.fontSize": 15,
                  "editor.tabSize": 2,
                  "editor.formatOnSave": true,
                  "workbench.colorTheme": "Night Owl",
                }"#,
            ),
            (
                "Library/Application Support/Code/User/keybindings.json",
                r#"[{"key": "ctrl+alt+d", "command": "editor.action.copyLinesDownAction"},
                    {"key": "cmd+k z", "command": "workbench.action.toggleZenMode"}]"#,
            ),
            (
                ".vscode/extensions/sdras.night-owl-2.0.1/package.json",
                r#"{"contributes": {"themes": [{"label": "Night Owl", "uiTheme": "vs-dark", "path": "./owl.json"}]}}"#,
            ),
            (
                ".vscode/extensions/sdras.night-owl-2.0.1/owl.json",
                r##"{"colors": {"editor.background": "#011627", "editor.foreground": "#d6deeb"},
                    "tokenColors": [{"scope": "keyword", "settings": {"foreground": "#c792ea"}}]}"##,
            ),
            (
                ".vscode/extensions/eamodio.gitlens-15.0.0/package.json",
                "{}",
            ),
        ] {
            let path = home.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        import::Roots {
            home,
            bundled: Vec::new(),
        }
    }

    #[gpui::test]
    fn import_brings_settings_a_theme_and_keys_from_another_editor(cx: &mut TestAppContext) {
        let root = fixture("import");
        let roots = vscode_home("import");
        // Solder's own config, where the user already chose a font size.
        let config = db::testing::dir("ws-import-config");
        std::fs::write(
            config.join("settings.json"),
            "// Mine.\n{\n  \"buffer_font_size\": 16\n}\n",
        )
        .unwrap();
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        ws.update_in(cx, |w, window, cx| {
            w.add_editor(Some(root.join("x.txt")), "one\ntwo", None, window, cx);
            w.show_import(roots, config.clone(), window, cx)
        });
        let view = cx.read(|cx| {
            let modal = ws.read(cx).modal.as_ref().expect("the Import window");
            modal
                .view
                .clone()
                .downcast::<crate::import_view::ImportView>()
                .unwrap()
        });
        wait_for(cx, "the other editor", &|cx| view.read(cx).loaded());
        cx.run_until_parked();

        // Rows: SETTINGS, font size (kept: the user's own), indent, format
        // on save, THEME, Night Owl, KEYS, VS Code keys, own bindings,
        // ALREADY BUILT IN, gitlens. The indent is left out by a click.
        let indent = cx.debug_bounds("import-item-2").expect("the indent row");
        cx.simulate_click(indent.center(), gpui::Modifiers::default());
        let apply = cx.debug_bounds("import-apply").expect("the Import button");
        cx.simulate_click(apply.center(), gpui::Modifiers::default());
        wait_for(cx, "the import", &|cx| view.read(cx).status().is_some());
        let status = cx.read(|cx| view.read(cx).status().unwrap());
        assert!(
            status.starts_with("Imported from VS Code: 1 setting, the theme Night Owl, "),
            "{status}"
        );

        // The user's file keeps its comment and its own value.
        let text = std::fs::read_to_string(config.join("settings.json")).unwrap();
        assert!(text.starts_with("// Mine.\n{"), "{text}");
        assert!(config.join("themes/night-owl.json").is_file());
        // The same folder, read as the app reads its config.
        cx.update(|_, cx| settings::reload_from(&config, cx));
        let settings = cx.read(|cx| Settings::get(cx).clone());
        assert_eq!(settings.buffer_font_size, 16.);
        assert_eq!(settings.indent_size, 4);
        assert!(settings.format_on_save);
        assert_eq!(
            settings.theme,
            settings::ThemeMode::Named("Night Owl".into())
        );
        let theme = settings.theme(gpui::WindowAppearance::Light);
        assert_eq!(theme.bg, gpui::Hsla::from(gpui::rgb(0x011627)));
        assert_eq!(theme.syntax.keyword, gpui::Hsla::from(gpui::rgb(0xc792ea)));
        assert!(cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));

        // The imported binding works; the one without an equivalent did
        // not come.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).modal.is_none()));
        cx.simulate_keystrokes("ctrl-alt-d");
        assert_eq!(active_text(&ws, cx), "one\none\ntwo");
        // The user's own keymap wins over what was imported.
        std::fs::write(
            config.join("keymap.json"),
            r#"[{"context": "Editor && mode == full", "bindings": {"ctrl-alt-d": "editor::SelectLine"}}]"#,
        )
        .unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        cx.simulate_keystrokes("ctrl-alt-d backspace");
        assert_eq!(active_text(&ws, cx), "one\ntwo");
        // Back to the defaults for the tests that follow.
        cx.update(|_, cx| settings::reload_from(&db::testing::dir("ws-import-none"), cx));
    }

    #[gpui::test]
    fn every_action_an_import_binds_exists(cx: &mut TestAppContext) {
        let (_ws, cx) = setup(cx, fixture("import-actions"));
        for (action, context) in import::keymap::actions() {
            cx.update(|_, cx| {
                assert!(cx.build_action(action, None).is_ok(), "{action}");
                if let Some(context) = context {
                    assert!(
                        gpui::KeyBindingContextPredicate::parse(context).is_ok(),
                        "{context}"
                    );
                }
            });
        }
        // The presets, as a keymap file, load without a complaint.
        for preset in [
            import::keymap::vscode_preset(true),
            import::keymap::vscode_preset(false),
            import::keymap::jetbrains_preset(true),
            import::keymap::jetbrains_preset(false),
        ] {
            let text = import::keymap::to_json(&preset);
            let (bindings, errors) = cx.update(|_, cx| settings::parse_keymap("preset", &text, cx));
            assert!(errors.is_empty(), "{errors:?}");
            assert!(bindings.len() > 30);
        }
    }

    #[gpui::test]
    fn a_plugin_written_in_typescript_runs_like_any_other(cx: &mut TestAppContext) {
        let (_root, data, store, ws, cx) = plugin_setup(cx, "plugin-ts", |plugins| {
            plugin::testing::install_script(&plugin::testing::word_count_ts(), plugins);
        });
        // Listed as a script, and off until enabled.
        assert!(cx.read(|cx| store.read(cx).find("word-count-ts").unwrap().script));
        assert_eq!(
            cx.read(|cx| store.read(cx).state("word-count-ts")),
            PluginState::Disabled
        );
        store.update(cx, |s, cx| s.enable("word-count-ts", cx));
        wait_for(cx, "the count", &|cx| {
            plugin_status(&store, "word-count-ts", cx).as_deref() == Some("3 words")
        });

        // Typing reaches it; positions it edits by are JavaScript's, in a
        // text with characters of two and four bytes.
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        editor.update_in(cx, |e, _, cx| {
            let end = e.text(cx).len();
            e.select_range(end..end, cx)
        });
        cx.simulate_input(" né 😀 ");
        wait_for(cx, "the new count", &|cx| {
            plugin_status(&store, "word-count-ts", cx).as_deref() == Some("5 words")
        });
        store.update(cx, |s, _| s.run_command("word-count-ts", "insert"));
        wait_for(cx, "the insertion", &|cx| {
            editor.read(cx).text(cx) == "one two three né 😀 5 words"
        });

        // The script is what was approved: another one stays off.
        std::fs::write(
            data.join("plugins/word-count-ts/plugin.js"),
            "solder.on('open', () => solder.status('replaced'));",
        )
        .unwrap();
        store.update(cx, |s, cx| s.scan(cx));
        wait_for(cx, "the change", &|cx| {
            store.read(cx).state("word-count-ts") == PluginState::Changed
        });
        assert_eq!(
            cx.read(|cx| plugin_status(&store, "word-count-ts", cx)),
            None
        );
    }

    #[gpui::test]
    fn a_plugin_written_in_go_runs_like_any_other(cx: &mut TestAppContext) {
        let mut installed = false;
        let (_root, _data, store, ws, cx) = plugin_setup(cx, "plugin-go", |plugins| {
            let go = plugin::testing::word_count_go();
            installed = plugin::testing::install_go(&go, plugins).is_some();
        });
        if !installed {
            eprintln!("go is not installed; skipped");
            return;
        }
        // A module like any other, though it is a program inside.
        assert!(!cx.read(|cx| store.read(cx).find("word-count-go").unwrap().script));
        store.update(cx, |s, cx| s.enable("word-count-go", cx));
        wait_for(cx, "the count", &|cx| {
            plugin_status(&store, "word-count-go", cx).as_deref() == Some("3 words")
        });
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        editor.update_in(cx, |e, _, cx| {
            let end = e.text(cx).len();
            e.select_range(end..end, cx)
        });
        cx.simulate_input(" né 😀 ");
        wait_for(cx, "the new count", &|cx| {
            plugin_status(&store, "word-count-go", cx).as_deref() == Some("5 words")
        });
        // Its positions are bytes, as the editor's are.
        store.update(cx, |s, _| s.run_command("word-count-go", "insert"));
        wait_for(cx, "the insertion", &|cx| {
            editor.read(cx).text(cx) == "one two three né 😀 5 words"
        });
        store.update(cx, |s, cx| s.disable("word-count-go", cx));
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| plugin_status(&store, "word-count-go", cx)),
            None
        );
    }

    // ----------------------------------------------------------- extensions

    use crate::extension_store::ExtensionStore;
    use extension::{
        Origin,
        testing::{Served, serve, tar, write as write_file, zip},
    };

    /// A store that keeps extensions in a fresh folder and asks the catalogs
    /// at `base`, a project with `App.vue` open, and the window.
    fn extension_setup<'a>(
        cx: &'a mut TestAppContext,
        name: &str,
        base: &str,
    ) -> (
        PathBuf,
        Entity<ExtensionStore>,
        Entity<Workspace>,
        &'a mut VisualTestContext,
    ) {
        let root = db::testing::dir(&format!("ws-{name}"))
            .canonicalize()
            .unwrap();
        let component = "<template>\n  <p>{{ msg }}</p>\n</template>\n<script setup lang=\"ts\">\nconst msg = 'hi'\n</script>\n";
        std::fs::write(root.join("App.vue"), component).unwrap();
        let data = db::testing::dir(&format!("ws-{name}-data"));
        let config = db::testing::dir(&format!("ws-{name}-config"));
        cx.executor().allow_parking();
        let store = cx.update(|cx| {
            let store = cx.new(|cx| {
                let mut store = ExtensionStore::new(data.join("extensions"), config.clone(), cx);
                store.zed_url = base.to_string();
                store.open_vsx_url = base.to_string();
                store
            });
            ExtensionStore::set_global(store.clone(), cx);
            store.update(cx, |s, cx| s.scan(cx));
            store
        });
        let (ws, cx) = setup(cx, root.clone());
        let file = root.join("App.vue");
        ws.update_in(cx, |w, window, cx| w.open_path(file, None, window, cx));
        wait_for(cx, "the extensions folder", &|cx| {
            store.read(cx).loaded && ws.read(cx).active_editor().is_some()
        });
        (config, store, ws, cx)
    }

    /// Zed's Vue extension as its catalog serves it: the real grammar and
    /// queries, plus a theme and a snippet file.
    fn vue_archive(name: &str) -> Vec<u8> {
        let dir = db::testing::dir(&format!("ws-{name}-archive")).join("vue");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../syntax/tests/fixtures/vue");
        write_file(
            &dir.join("extension.toml"),
            "id = \"vue\"\nname = \"Vue\"\nversion = \"0.4.0\"\nschema_version = 1\ndescription = \"Vue support.\"\n",
        );
        write_file(
            &dir.join("languages/vue/config.toml"),
            "name = \"Vue.js\"\ngrammar = \"vue\"\npath_suffixes = [\"vue\"]\ncode_fence_block_name = \"vue\"\n",
        );
        for query in ["highlights.scm", "injections.scm"] {
            std::fs::copy(fixtures.join(query), dir.join("languages/vue").join(query)).unwrap();
        }
        std::fs::create_dir_all(dir.join("grammars")).unwrap();
        std::fs::copy(fixtures.join("vue.wasm"), dir.join("grammars/vue.wasm")).unwrap();
        write_file(&dir.join("themes/demo.json"), extension::testing::ZED_THEME);
        write_file(
            &dir.join("snippets/vue.json"),
            r#"{"Base": {"prefix": "vbase", "body": ["<section>", "\t${1:$TM_FILENAME_BASE}", "</section>"], "description": "A section"}}"#,
        );
        tar(&dir)
    }

    /// The languages of extensions are one set per process, and tests run in
    /// parallel in one process: a test that installs a language holds this
    /// for as long as it runs, so another does not replace the set under it.
    fn extension_languages() -> std::sync::MutexGuard<'static, ()> {
        static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        // Start from what a fresh install has: none, whatever the test
        // before left installed.
        syntax::set_extension_languages(Vec::new());
        guard
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("no {selector} on screen"));
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_zed_extension_installs_from_the_catalog_and_works(cx: &mut TestAppContext) {
        let _languages = extension_languages();
        let catalog = r#"{"data":[{"id":"vue","name":"Vue","version":"0.4.0","description":"Vue support.","download_count":675263,"provides":["languages","grammars","language-servers"]}]}"#;
        // The catalog lists the version the archive holds, and says a newer
        // one is out when it is asked about what is installed.
        let newest = r#"{"data":[{"id":"vue","name":"Vue","version":"0.5.0","description":"Vue support.","download_count":675263,"provides":["languages"]}]}"#;
        let (base, requests) = serve(vec![
            ("/extensions", Served::ok(catalog.as_bytes().to_vec())),
            (
                "/extensions/updates",
                Served::ok(newest.as_bytes().to_vec()),
            ),
            (
                "/extensions/vue/download",
                Served::ok(vue_archive("ext-zed")),
            ),
        ]);
        let (config, store, ws, cx) = extension_setup(cx, "ext-zed", &base);
        let folder = cx.read(|cx| store.read(cx).root.clone());
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let language = |cx: &App| editor.read(cx).doc(cx).language_name();
        // Nothing installed: the file is plain text.
        assert_eq!(cx.read(|cx| language(cx)), None);

        // The catalogs are asked when the tab is opened, not before.
        assert!(requests.lock().unwrap().is_empty());
        click(cx, "sidebar-extensions");
        assert!(cx.read(|cx| ws.read(cx).sidebar == Some(SidebarTab::Extensions)));
        wait_for(cx, "the catalogs", &|cx| {
            let store = store.read(cx);
            !store.catalog(Origin::Zed).entries.is_empty() && store.catalog(Origin::VsCode).searched
        });
        cx.run_until_parked();
        // Both were asked for what they have most of. Open VSX is not there
        // in this test: the list still shows what Zed's catalog answered.
        {
            let asked = requests.lock().unwrap();
            assert!(asked.iter().any(|r| r.starts_with("/extensions?")));
            assert!(asked.iter().any(|r| r.starts_with("/api/-/search?")));
        }
        assert!(cx.read(|cx| store.read(cx).catalog(Origin::VsCode).error.is_some()));
        click(cx, "extension-act-0");
        wait_for(cx, "the install", &|cx| {
            store.read(cx).find(Origin::Zed, "vue").is_some()
        });
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx)));

        // The file that was already open is now Vue, with its script
        // highlighted as TypeScript.
        wait_for(cx, "the grammar", &|cx| {
            editor.read(cx).doc(cx).syntax().is_some()
        });
        assert_eq!(cx.read(|cx| language(cx)), Some("Vue.js"));
        let spans = cx.read(|cx| {
            let doc = editor.read(cx).doc(cx);
            let rope = doc.text().rope();
            let text = rope.to_string();
            doc.syntax()
                .unwrap()
                .highlights(rope, 0..rope.len_bytes())
                .into_iter()
                .map(|(r, k)| (text[r].to_string(), k))
                .collect::<Vec<_>>()
        });
        assert!(spans.contains(&("template".into(), syntax::HighlightKind::Tag)));
        assert!(spans.contains(&("const".into(), syntax::HighlightKind::Keyword)));

        // Its snippet completes, with the file's name filled in.
        editor.update_in(cx, |e, _, cx| {
            let end = e.text(cx).len();
            e.select_range(end..end, cx)
        });
        cx.simulate_input("vba");
        wait_for(cx, "the snippet", &|cx| {
            editor.read(cx).completion.is_some()
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let (text, selected) = cx.read(|cx| {
            let editor = editor.read(cx);
            let text = editor.text(cx);
            let range = editor.newest_range();
            (text.clone(), text[range].to_string())
        });
        assert!(text.ends_with("<section>\n  App\n</section>"), "{text:?}");
        assert_eq!(selected, "App");

        // One of its themes becomes the editor's. The extension is the
        // first row now, and the catalog's record of it is not repeated.
        cx.dispatch_action(ShowExtensions);
        cx.run_until_parked();
        assert!(cx.debug_bounds("extension-0").is_some());
        assert!(cx.debug_bounds("extension-1").is_none());
        click(cx, "extension-theme-0");
        wait_for(cx, "the theme", &|cx| {
            store.read(cx).theme_status == Some(Ok("Demo Dark".into()))
        });
        assert_eq!(
            cx.read(|cx| cx.global::<Settings>().theme.clone()),
            settings::ThemeMode::Named("Demo Dark".into())
        );
        assert!(config.join("themes/demo-dark.json").is_file());

        // Opening the tab asked the catalog about what is installed, and it
        // has a newer version: the row offers it, and so does Update all.
        let has_update = |cx: &App| {
            store
                .read(cx)
                .updates
                .contains_key(&(Origin::Zed, "vue".into()))
        };
        wait_for(cx, "the update", &has_update);
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.starts_with("/extensions/updates?") && r.ends_with("&ids=vue"))
        );
        cx.run_until_parked();
        assert!(cx.debug_bounds("extensions-update-all").is_some());
        // Kept on its version, it is offered no update; following again,
        // the catalog is asked and offers it.
        click(cx, "extension-keep");
        assert!(!cx.read(|cx| has_update(cx)));
        click(cx, "extension-keep");
        wait_for(cx, "the update again", &has_update);

        // Turned off, it stays installed and its language is gone; turned
        // on, the language is back. The decision is written down.
        click(cx, "extension-off");
        wait_for(cx, "the language to go", &|cx| language(cx).is_none());
        assert!(cx.read(|cx| store.read(cx).find(Origin::Zed, "vue").is_some()));
        wait_for(cx, "the decision on disk", &|_| {
            std::fs::read_to_string(folder.join("state.json"))
                .is_ok_and(|text| text.contains("\"zed/vue\"") && text.contains("\"off\": true"))
        });
        click(cx, "extension-off");
        wait_for(cx, "the language to return", &|cx| {
            language(cx) == Some("Vue.js")
        });

        // Update all downloads it again.
        let downloads = |requests: &std::sync::Mutex<Vec<String>>| {
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.starts_with("/extensions/vue/download"))
                .count()
        };
        assert_eq!(downloads(&requests), 1);
        click(cx, "extensions-update-all");
        wait_for(cx, "the update to install", &|cx| {
            downloads(&requests) == 2 && store.read(cx).installing.is_empty()
        });

        // Removed, the file is plain text again, and nothing is remembered
        // about the extension.
        cx.run_until_parked();
        click(cx, "extension-remove");
        wait_for(cx, "the removal", &|cx| {
            store.read(cx).loaded && store.read(cx).installed.is_empty()
        });
        assert_eq!(cx.read(|cx| language(cx)), None);
        assert!(cx.read(|cx| editor.read(cx).doc(cx).syntax().is_none()));
        wait_for(cx, "the decisions to be forgotten", &|_| {
            std::fs::read_to_string(folder.join("state.json"))
                .is_ok_and(|text| !text.contains("vue"))
        });
    }

    #[gpui::test]
    fn a_vscode_extension_says_what_does_not_run_and_points_to_zed(cx: &mut TestAppContext) {
        let dir = db::testing::dir("ws-ext-vsx-archive");
        extension::testing::vscode_extension(&dir.join("extension"));
        let manifest = std::fs::read_to_string(dir.join("extension/package.json"))
            .unwrap()
            .replace("\"publisher\": \"Acme\"", "\"publisher\": \"Vue\"")
            .replace("\"name\": \"demo\"", "\"name\": \"volar\"");
        std::fs::write(dir.join("extension/package.json"), manifest).unwrap();
        let Some(vsix) = zip(&dir, "extension") else {
            eprintln!("skipped: no python3 to build a .vsix");
            return;
        };
        // The catalog's answer names the download by its full address, so
        // the file is served first, from a server of its own.
        let (files, _) = serve(vec![("/volar.vsix", Served::ok(vsix))]);
        let search = format!(
            r#"{{"extensions":[{{"namespace":"Vue","name":"volar","displayName":"Vue (Official)","version":"3.0.1","description":"Language support for Vue","downloadCount":5200000,"files":{{"download":"{files}/volar.vsix"}}}}]}}"#
        );
        let (base, requests) = serve(vec![
            ("/api/-/search", Served::ok(search.into_bytes())),
            ("/extensions", Served::ok(br#"{"data":[]}"#.to_vec())),
        ]);
        let (_config, store, ws, cx) = extension_setup(cx, "ext-vsx", &base);

        cx.dispatch_action(ShowExtensions);
        wait_for(cx, "the catalogs", &|cx| {
            let store = store.read(cx);
            !store.catalog(Origin::VsCode).entries.is_empty() && store.catalog(Origin::Zed).searched
        });
        cx.run_until_parked();
        // Enter installs what is selected.
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the install", &|cx| {
            store.read(cx).find(Origin::VsCode, "vue.volar").is_some()
        });
        let installed = cx.read(|cx| store.read(cx).find(Origin::VsCode, "Vue.volar").cloned());
        let installed = installed.unwrap();
        assert_eq!(installed.themes.len(), 1);
        assert!(
            installed
                .missing
                .iter()
                .any(|m| m.contains("needs VS Code"))
        );
        // Its language (files ending in .dm) has no grammar here, so it is
        // not a language of the editor. Asked of the registry and not of the
        // open file: the registry is one per process, and the test next to
        // this one installs Vue into it.
        assert_eq!(installed.languages[0].suffixes, ["dm", "Demofile"]);
        assert!(syntax::language_for_path(Path::new("notes.dm")).is_none());
        assert!(cx.read(|cx| ws.read(cx).sidebar == Some(SidebarTab::Extensions)));

        // The tab points to the Zed extension for the language and searches
        // for it.
        cx.run_until_parked();
        click(cx, "extension-equivalent");
        wait_for(cx, "Zed's catalog", &|cx| {
            let catalog = store.read(cx).catalog(Origin::Zed);
            catalog.searched && catalog.query == "vue"
        });
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r == "/extensions?max_schema_version=1&filter=vue")
        );
    }

    /// What extensions reach outside their sandbox through, for the test
    /// below: the server is "on the PATH" as a script that starts the mock
    /// language server, and nothing else is available.
    struct ServerOnPath(String);

    impl extension::host::World for ServerOnPath {
        fn node(&self) -> Result<String, String> {
            Err("no Node here".into())
        }
        fn npm_latest(&self, _: &str) -> Result<String, String> {
            Err("no npm here".into())
        }
        fn npm_install(&self, _: &Path, _: &str, _: &str) -> Result<(), String> {
            Err("no npm here".into())
        }
        fn release(
            &self,
            _: &str,
            _: Option<&str>,
            _: bool,
        ) -> Result<extension::host::Release, String> {
            Err("no network here".into())
        }
        fn download(&self, _: &str, _: &Path, _: extension::host::FileKind) -> Result<(), String> {
            Err("no network here".into())
        }
        fn fetch(
            &self,
            _: extension::host::HttpRequest,
        ) -> Result<extension::host::HttpResponse, String> {
            Err("no network here".into())
        }
        fn run(&self, _: &extension::host::Command) -> Result<extension::host::Output, String> {
            Err("no commands here".into())
        }
        fn which(&self, binary: &str) -> Option<String> {
            (binary == "vscode-html-language-server").then(|| self.0.clone())
        }
        fn env(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn status(&self, _: &str, _: extension::host::Status) {}
    }

    #[gpui::test]
    fn a_language_server_of_a_zed_extension_starts_for_its_language(cx: &mut TestAppContext) {
        let _languages = extension_languages();
        // An extension whose language is the Vue of the fixtures and whose
        // code is Zed's real HTML extension: asked for its server, it looks
        // on the PATH first, where the test has put the mock server.
        let source = db::testing::dir("ws-ext-lsp-source");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        write_file(
            &source.join("extension.toml"),
            "id = \"html\"\nname = \"HTML\"\nversion = \"0.3.2\"\nschema_version = 1\n\n[lib]\nkind = \"Rust\"\nversion = \"0.7.0\"\n\n[language_servers.vscode-html-language-server]\nlanguage = \"Vue.js\"\n\n[language_servers.vscode-html-language-server.language_ids]\n\"Vue.js\" = \"html\"\n",
        );
        std::fs::copy(
            fixtures.join("extension/tests/fixtures/html/extension.wasm"),
            source.join("extension.wasm"),
        )
        .unwrap();
        write_file(
            &source.join("languages/vue/config.toml"),
            "name = \"Vue.js\"\ngrammar = \"vue\"\npath_suffixes = [\"vue\"]\n",
        );
        for query in ["highlights.scm", "injections.scm"] {
            std::fs::copy(
                fixtures.join("syntax/tests/fixtures/vue").join(query),
                source.join("languages/vue").join(query),
            )
            .unwrap();
        }
        std::fs::create_dir_all(source.join("grammars")).unwrap();
        std::fs::copy(
            fixtures.join("syntax/tests/fixtures/vue/vue.wasm"),
            source.join("grammars/vue.wasm"),
        )
        .unwrap();
        let catalog = r#"{"data":[{"id":"html","name":"HTML","version":"0.3.2","description":"HTML support.","download_count":1,"provides":["languages","language-servers"]}]}"#;
        let (base, _) = serve(vec![
            ("/extensions", Served::ok(catalog.as_bytes().to_vec())),
            ("/extensions/html/download", Served::ok(tar(&source))),
        ]);

        // The server "on the PATH": a script that starts the mock server
        // and tells it where to write down what it is sent.
        let scratch = db::testing::dir("ws-ext-lsp-server");
        let log = scratch.join("sent.jsonl");
        let script = scratch.join("vscode-html-language-server");
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_lsp.py");
        write_file(
            &script,
            &format!(
                "#!/bin/sh\nMOCK_LSP_LOG='{}' exec python3 '{}'\n",
                log.display(),
                mock.display()
            ),
        );
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let (_config, store, ws, cx) = extension_setup(cx, "ext-lsp", &base);
        let world = ServerOnPath(script.to_string_lossy().into_owned());
        store.update(cx, |store, _| {
            store.world = Some(std::sync::Arc::new(world))
        });
        let lsp = cx.read(|cx| LspStore::global(cx).unwrap());
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());

        // Installing the extension is all it takes: the open file gets its
        // language, and the language its server.
        cx.dispatch_action(ShowExtensions);
        // Searching while the catalogs are still answering the tab's first
        // question asks them again: the answer that counts is the one to
        // what is typed now.
        let panel = cx.read(|cx| ws.read(cx).extensions_panel.clone());
        panel.update(cx, |panel, cx| panel.search_for("html", cx));
        wait_for(cx, "the catalog", &|cx| {
            let catalog = store.read(cx).catalog(Origin::Zed);
            catalog.searched && catalog.query == "html" && !catalog.entries.is_empty()
        });
        cx.run_until_parked();
        // This extension brings a language server, so it is downloaded and
        // then waits: nothing is installed until what it would do is read
        // and allowed.
        let folder = cx.read(|cx| store.read(cx).root.clone());
        let waiting = |cx: &App| {
            store
                .read(cx)
                .pending
                .contains_key(&(Origin::Zed, "html".into()))
        };
        click(cx, "extension-act-0");
        wait_for(cx, "the download", &waiting);
        assert!(cx.read(|cx| store.read(cx).find(Origin::Zed, "html").is_none()));
        assert_eq!(
            cx.read(|cx| store.read(cx).asks(Origin::Zed, "html")),
            ["Download and start the language server vscode-html-language-server"]
        );
        // Refused, the download is dropped and nothing was installed.
        cx.run_until_parked();
        click(cx, "extension-refuse");
        assert!(!cx.read(|cx| waiting(cx)));
        wait_for(cx, "the download to be dropped", &|_| {
            !folder.join(".staging/zed-html").exists()
        });
        assert!(!folder.join("zed/html").exists());
        // Asked for again and allowed, it is installed.
        click(cx, "extension-act-0");
        wait_for(cx, "the second download", &waiting);
        cx.run_until_parked();
        click(cx, "extension-allow");
        wait_for(cx, "the install", &|cx| {
            store.read(cx).find(Origin::Zed, "html").is_some()
        });
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx)));
        wait_for(cx, "the language server", &|cx| {
            !lsp.read(cx)
                .all_capabilities(editor.read(cx).document())
                .is_empty()
        });
        assert_eq!(cx.read(|cx| lsp.read(cx).status().cloned()), None);

        // The server was started with what the extension said: its
        // initialization options, and the language under the id the
        // extension's manifest gives it.
        let sent = |what: &str| -> Option<serde_json::Value> {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            text.lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .find(|entry| entry[0] == what)
                .map(|entry| entry[1].clone())
        };
        wait_for(cx, "the file to be opened in the server", &|_| {
            sent("open").is_some()
        });
        assert_eq!(
            sent("initialize"),
            Some(serde_json::json!({ "provideFormatter": true }))
        );
        assert_eq!(sent("open"), Some(serde_json::json!("html")));

        // It answers like any other server: completion works in the file.
        editor.update_in(cx, |e, _, cx| {
            let end = e.text(cx).len();
            e.select_range(end..end, cx)
        });
        cx.simulate_input("pri");
        wait_for(cx, "completions from the server", &|cx| {
            editor
                .read(cx)
                .completion
                .as_ref()
                .is_some_and(|menu| menu.items.iter().any(|item| item.label == "println"))
        });

        // The window counts the server among what Solder uses.
        let installed = cx.read(|cx| store.read(cx).find(Origin::Zed, "html").cloned());
        assert_eq!(
            installed.unwrap().provides(),
            "1 language, 1 language server"
        );
    }

    #[gpui::test]
    fn a_file_with_two_language_servers_gets_the_answers_of_both(cx: &mut TestAppContext) {
        // Rust has a server Solder knows; here it is the mock. An installed
        // extension brings a second one for the language: Zed's real HTML
        // extension, whose code finds "its" server on the PATH, where the
        // test has put the mock again, tagged so its answers can be told
        // apart, and counting columns in UTF-16 where the first counts bytes.
        let root = db::testing::dir("ws-two-servers").canonicalize().unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src/main.rs");
        std::fs::write(&file, "fn helper() {}\n// TODO one\n// FIXME two\n").unwrap();
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_lsp.py");
        let scratch = db::testing::dir("ws-two-servers-second");
        let script = scratch.join("vscode-html-language-server");
        write_file(
            &script,
            &format!(
                "#!/bin/sh\nMOCK_LSP_TAG=second exec python3 '{}'\n",
                mock.display()
            ),
        );
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let data = db::testing::dir("ws-two-servers-data");
        let installed = data.join("extensions/zed/second");
        write_file(
            &installed.join("extension.toml"),
            "id = \"second\"\nname = \"Second\"\nversion = \"1.0.0\"\nschema_version = 1\n\n[lib]\nkind = \"Rust\"\nversion = \"0.7.0\"\n\n[language_servers.vscode-html-language-server]\nlanguage = \"Rust\"\n",
        );
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../extension/tests/fixtures/html/extension.wasm"),
            installed.join("extension.wasm"),
        )
        .unwrap();
        cx.executor().allow_parking();
        let store = cx.update(|cx| {
            let store = cx.new(|cx| {
                let mut store =
                    ExtensionStore::new(data.join("extensions"), data.join("config"), cx);
                let world = ServerOnPath(script.to_string_lossy().into_owned());
                store.world = Some(std::sync::Arc::new(world));
                store
            });
            ExtensionStore::set_global(store.clone(), cx);
            store.update(cx, |s, cx| s.scan(cx));
            store
        });
        let (ws, cx) = setup(cx, root.clone());
        cx.update(|_, cx| {
            let mut settings = Settings::default();
            settings.language_servers.insert(
                "rust-analyzer".into(),
                settings::ServerOverride {
                    command: Some("python3".into()),
                    args: Some(vec![mock.display().to_string()]),
                    ..Default::default()
                },
            );
            cx.set_global(settings);
        });
        wait_for(cx, "the extensions folder", &|cx| store.read(cx).loaded);
        ws.update_in(cx, |w, window, cx| {
            w.open_path(file.clone(), None, window, cx)
        });
        wait_for(cx, "the file", &|cx| ws.read(cx).active_editor().is_some());
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let lsp = cx.read(|cx| LspStore::global(cx).unwrap());
        wait_for(cx, "both servers", &|cx| {
            lsp.read(cx)
                .all_capabilities(editor.read(cx).document())
                .len()
                == 2
        });

        // Each reports its own, and the file shows both, in order.
        let reported = |cx: &App| -> Vec<(String, &'static str)> {
            editor
                .read(cx)
                .doc(cx)
                .diagnostics()
                .iter()
                .map(|d| (d.message.clone(), d.server))
                .collect()
        };
        wait_for(cx, "the diagnostics of both", &|cx| reported(cx).len() == 2);
        assert_eq!(
            cx.read(|cx| reported(cx)),
            [
                ("found TODO".to_string(), "rust-analyzer"),
                ("found FIXME".to_string(), "vscode-html-language-server"),
            ]
        );
        // One of them changing its mind leaves the other's in place: the
        // word the second reports is edited away.
        editor.update_in(cx, |e, _, cx| {
            let at = e.text(cx).find("FIXME").unwrap();
            e.select_range(at..at + 5, cx)
        });
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx)));
        cx.simulate_input("LATER");
        wait_for(cx, "the second to take its report back", &|cx| {
            reported(cx) == [("found TODO".to_string(), "rust-analyzer")]
        });

        // Completions come from both in one menu.
        editor.update_in(cx, |e, _, cx| {
            let end = e.text(cx).len();
            e.select_range(end..end, cx)
        });
        cx.simulate_input("pri");
        wait_for(cx, "completions of both", &|cx| {
            editor.read(cx).completion.as_ref().is_some_and(|menu| {
                let has = |label: &str| menu.items.iter().any(|item| item.label == label);
                has("println") && has("second_println")
            })
        });
        cx.simulate_keystrokes("escape");

        // Code actions are listed together, and one that is picked goes
        // back to the server it came from to be filled in.
        cx.simulate_keystrokes("secondary-.");
        wait_for(cx, "code actions", &|cx| ws.read(cx).modal.is_some());
        cx.simulate_input("second add");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the second server's edit", &|cx| {
            editor.read(cx).text(cx).starts_with("// header second\n")
        });
        cx.simulate_keystrokes("secondary-.");
        wait_for(cx, "code actions again", &|cx| ws.read(cx).modal.is_some());
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the first server's edit", &|cx| {
            editor
                .read(cx)
                .text(cx)
                .starts_with("// header\n// header second\n")
        });

        // Turning the extension off leaves the file with the first server
        // only, and takes the second's reports with it.
        editor.update_in(cx, |e, _, cx| {
            let end = e.text(cx).len();
            e.select_range(end..end, cx)
        });
        cx.simulate_input("\n// FIXME again\n");
        wait_for(cx, "a report of the second", &|cx| {
            reported(cx)
                .iter()
                .any(|(_, server)| *server == "vscode-html-language-server")
        });
        store.update(cx, |store, cx| {
            store.set_off(Origin::Zed, "second", true, cx)
        });
        wait_for(cx, "the second server to leave the file", &|cx| {
            lsp.read(cx)
                .all_capabilities(editor.read(cx).document())
                .len()
                == 1
                && reported(cx)
                    .iter()
                    .all(|(_, server)| *server == "rust-analyzer")
        });
    }
}
