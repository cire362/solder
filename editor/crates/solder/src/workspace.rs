use std::{
    any::TypeId,
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyElement, AnyView, App, Context, DismissEvent, Entity, EntityId, FocusHandle, Focusable,
    KeyBinding, ManagedView, MouseButton, MouseDownEvent, MouseMoveEvent, PathPromptOptions,
    Pixels, Point, PromptLevel, SharedString, Subscription, Task, Window, WindowControlArea,
    actions, anchored, deferred, div, prelude::*, px,
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
    extension_store::ExtensionStore,
    file_diff::{DiffModel, FileDiff, FileDiffEvent},
    file_finder::FileFinder,
    git::DiffScope,
    git_panel::{self, BranchPicker, GitPanel, GitPanelEvent},
    git_store::{GitStore, GitStoreEvent},
    go_to_line::GoToLine as GoToLineDelegate,
    inline_edit::{InlineEdit, InlineEditEvent},
    key_layout_picker::{KeyLayoutPicker, SwitchKeyLayout},
    layout::{self, BarEnd, Display, Item, Layout, Panel, Part, Place, TabBar, TabsAt},
    layout_picker::{LayoutPicker, SwitchLayout},
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
    theme::{ActiveTheme, UI_FONT_SIZE},
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
        OpenLayout,
        SaveLayout,
        SaveKeyLayout,
        OpenKeyLayout,
        ResetLayout,
        UseContextPrompt,
        RunExtensionTask,
        GoToSymbol,
        GoToProjectSymbol,
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
        ShowExtensionViews,
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
        KeyBinding::new("secondary-shift-o", GoToSymbol, None),
        KeyBinding::new("secondary-t", GoToProjectSymbol, None),
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
    web_pages: Vec<(Entity<crate::webview::Page>, Subscription)>,
    web_front: Option<(Entity<crate::webview::Page>, usize)>,
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
    extension_views: Entity<crate::extension_views::ExtensionViews>,
    chat: Entity<crate::chat_panel::ChatPanel>,
    agent: Entity<crate::agent_panel::AgentPanel>,
    /// Pushes started, for tests: the terminal running one may be gone.
    #[cfg(test)]
    pushes: usize,
    inline_edit: Option<(Entity<InlineEdit>, Subscription)>,
    results: Entity<ResultsView>,
    /// The Results tab is in the dock (a query has run and it was not closed).
    show_results: bool,
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
    /// The panel each dock shows; `None` for a dock that is closed. Which
    /// dock a panel is in is the layout's to say.
    left: Option<Panel>,
    right: Option<Panel>,
    bottom: Option<Panel>,
    /// Where the focus is when it is anywhere in a dock, to know if
    /// closing the dock takes the focus with it.
    dock_focus: [FocusHandle; 3],
    _layout: Subscription,
    layout_selection: u64,
    /// A border being dragged: the part it sizes, where the pointer went
    /// down along the border's way, and the part's size then.
    resizing: Option<(Part, f32, f32)>,
    /// The menu of a dock: where it opened, the dock, and the panel whose
    /// tab was under the pointer, if one was.
    dock_menu: Option<(Point<Pixels>, Place, Option<Panel>)>,
    /// The terminals extensions made in this window's dock, each by the
    /// extension and the number it gave the terminal.
    extension_terminals: Vec<((String, u64), Entity<Terminal>)>,
    /// What extensions offer for the file in front, where the right
    /// button was pressed in it.
    editor_menu: Option<(Point<Pixels>, Vec<crate::extension_api::Offered>)>,
    bar_menu: Option<(Point<Pixels>, BarEnd, Option<Item>)>,
    /// The panel whose tab is being dragged. A closed dock has a place to
    /// drop it on for as long as it is.
    dragging: Option<Panel>,
    modal: Option<Modal>,
    terminals: Vec<(Entity<Terminal>, Subscription)>,
    active_terminal: usize,
    _subscriptions: Vec<Subscription>,
    _hud_tick: Task<()>,
}

/// What an item of a layout menu does.
type MenuRun = Box<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// A panel's tab while it is dragged to another place.
#[derive(Clone, Copy)]
struct DraggedPanel(Panel);

/// What follows the pointer then.
struct DraggedTab(Panel);

#[derive(Clone)]
struct DraggedItem(Item);

struct DraggedBarItem(Item);

struct BarPart {
    text: String,
    id: Option<(&'static str, usize)>,
    color: Option<gpui::Hsla>,
    action: Option<Box<dyn gpui::Action>>,
}

impl BarPart {
    fn render(self, clickable: bool, theme: &crate::theme::Theme) -> AnyElement {
        let d = div()
            .when_some(self.color, |d, color| d.text_color(color))
            .child(self.text);
        match self.action.filter(|_| clickable) {
            Some(action) => d
                .id(self.id.unwrap())
                .px_1p5()
                .rounded(theme.shape.token)
                .hover(|d| d.bg(theme.line).text_color(theme.fg))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
                .into_any_element(),
            None => d.into_any_element(),
        }
    }
}

impl Render for DraggedBarItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .h(crate::theme::row(px(24.), cx))
            .px_2()
            .flex()
            .items_center()
            .rounded(theme.shape.control)
            .bg(theme.bg_elev)
            .border(theme.shape.border)
            .border_color(theme.line)
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .gap_1p5()
            .child(crate::icons::draw(crate::icons::item(&self.0)))
            .child(self.0.label().to_owned())
    }
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .h(px(24.))
            .px_2()
            .flex()
            .items_center()
            .rounded(theme.shape.control)
            .bg(theme.bg_elev)
            .border(theme.shape.border)
            .border_color(theme.line)
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .gap_1p5()
            .child(crate::icons::draw(crate::icons::panel(self.0)))
            .child(self.0.label())
    }
}

impl Workspace {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        layout::for_project(&root, window, cx);
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
        let extensions = ExtensionStore::global(cx);
        // The code of extensions sees this window's folder, and asks the
        // user in the window in front.
        let this = cx.entity().downgrade();
        extensions.update(cx, |store, cx| store.set_front(this, root.clone(), cx));
        let extension_asks =
            cx.subscribe_in(
                &extensions,
                window,
                |this, _, event, window, cx| match event {
                    crate::extension_api::ExtensionEvent::Asked => this.extension_asks(window, cx),
                    crate::extension_api::ExtensionEvent::Webviews => this.sync_webviews(cx),
                    crate::extension_api::ExtensionEvent::Bar
                    | crate::extension_api::ExtensionEvent::Views
                    | crate::extension_api::ExtensionEvent::Files => cx.notify(),
                },
            );
        let extension_views =
            cx.new(|cx| crate::extension_views::ExtensionViews::new(extensions.clone(), cx));
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
                        .update(cx, |p, cx| p.set_workspace(weak.clone(), root.clone(), cx));
                    if let Some(extensions) = ExtensionStore::try_global(cx) {
                        extensions.update(cx, |store, cx| store.set_front(weak, root, cx));
                    }
                }
            }),
            extension_asks,
            cx.subscribe_in(&debug, window, |this, _, event, window, cx| match event {
                crate::debug::DebugEvent::Paused(path, line) => {
                    // The window whose project holds the file shows it.
                    if path.starts_with(this.root(cx)) {
                        this.show_debug = true;
                        this.show_panel(Panel::Debug, cx);
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
                        this.show_panel(Panel::Results, cx);
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
                                        ws.show_panel(Panel::Results, cx);
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
                crate::theme::put(theme, window, cx);
                window.refresh();
            }),
            // Settings changed: the theme mode may have too, and the size
            // of the interface's text.
            cx.observe_global_in::<Settings>(window, |_, window, cx| {
                let theme = Settings::get(cx).theme(window.appearance());
                crate::theme::put(theme, window, cx);
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
        crate::theme::fit(window, cx);
        // The docks start on the panels they were left on. The terminals,
        // the debugger and the answers have nothing to show yet, so a dock
        // left on one of them starts closed.
        let layout = Layout::get(cx);
        let [left, right, bottom] = Place::ALL.map(|place| {
            layout.open_in(place).filter(|panel| {
                !matches!(
                    panel,
                    Panel::Terminal | Panel::Debug | Panel::Response | Panel::Results
                )
            })
        });
        let mut this = Self {
            web_pages: Vec::new(),
            web_front: None,
            focus_handle: cx.focus_handle(),
            project,
            project_panel,
            project_search,
            search_bar,
            panes: vec![Pane::default()],
            active_pane: 0,
            recent: VecDeque::new(),
            left,
            right,
            bottom,
            dock_focus: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
            _layout: cx.observe_global_in::<Layout>(window, Self::follow_layout),
            layout_selection: layout::selection(cx),
            resizing: None,
            dock_menu: None,
            extension_terminals: Vec::new(),
            editor_menu: None,
            bar_menu: None,
            dragging: None,
            modal: None,
            terminals: Vec::new(),
            active_terminal: 0,
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
            extension_views,
            chat,
            agent,
            #[cfg(test)]
            pushes: 0,
            inline_edit: None,
            results,
            show_results: false,
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
        };
        for panel in [left, right, bottom] {
            this.tell_panels(panel, cx);
        }
        this
    }

    pub fn root(&self, cx: &App) -> PathBuf {
        self.project.read(cx).root().to_path_buf()
    }

    pub(crate) fn active_editor(&self) -> Option<&Entity<Editor>> {
        if self.web_front.is_some() {
            return None;
        }
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
        self.web_front = None;
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
        if let Some((page, _)) = self.web_front.clone() {
            self.close_webview(&page, window, cx);
            return;
        }
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
        self.set_sidebar(Some(Panel::Git), cx);
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
                // Services keep their tab so a crash's output stays readable,
                // and so do the tasks of extensions.
                TerminalEvent::Exited if terminal.read(cx).keep_on_exit => {
                    this.extension_terminal_gone(terminal, cx);
                    cx.notify()
                }
                TerminalEvent::Exited => this.remove_terminal(terminal, window, cx),
            },
        );
        self.terminals.push((terminal.clone(), subscription));
        self.active_terminal = self.terminals.len() - 1;
        self.show_panel(Panel::Terminal, cx);
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
            self.show_panel(Panel::Terminal, cx);
            window.focus(&terminal.focus_handle(cx));
            cx.notify();
        }
    }

    fn show_services(&mut self, _: &ShowServices, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(Panel::Services), cx);
        window.focus(&self.services.focus_handle(cx));
    }

    /// Whether `panel` is the one a dock shows now.
    fn shown(&self, panel: Panel) -> bool {
        [self.left, self.right, self.bottom].contains(&Some(panel))
    }

    /// Whether a panel has a tab now. Results, the last response and the
    /// debugger have one only while they have something to show, and the
    /// terminals while there is one.
    fn available(&self, panel: Panel) -> bool {
        match panel {
            Panel::Terminal => !self.terminals.is_empty(),
            Panel::Results => self.show_results,
            Panel::Response => self.show_response,
            Panel::Debug => self.show_debug,
            _ => true,
        }
    }

    /// A panel has nothing left to show. The dock that showed it shows
    /// the first of its panels that has, or closes.
    fn panel_gone(&mut self, panel: Panel, cx: &mut Context<Self>) {
        for place in Place::ALL {
            if *self.dock(place) == Some(panel) {
                let next = Layout::get(cx)
                    .panels(place)
                    .iter()
                    .copied()
                    .find(|other| *other != panel && self.available(*other));
                *self.dock(place) = next;
                self.docks_changed(next, cx);
            }
        }
    }

    /// Where the keyboard goes in a panel.
    fn panel_focus(&self, panel: Panel, cx: &App) -> Option<FocusHandle> {
        Some(match panel {
            Panel::Files => self.project_panel.focus_handle(cx),
            Panel::Search => self.project_search.focus_handle(cx),
            Panel::Git => self.git_panel.focus_handle(cx),
            Panel::Services => self.services.focus_handle(cx),
            Panel::Database => self.database_panel.focus_handle(cx),
            Panel::Api => self.api_panel.focus_handle(cx),
            Panel::Ai => self.ai_panel.focus_handle(cx),
            Panel::Extensions => self.extensions_panel.focus_handle(cx),
            Panel::ExtensionViews => self.extension_views.focus_handle(cx),
            Panel::Chat => self.chat.focus_handle(cx),
            Panel::Agent => self.agent.focus_handle(cx),
            Panel::Debug => self.debug_panel.focus_handle(cx),
            Panel::Response => self.response.focus_handle(cx),
            Panel::Results => self.results.focus_handle(cx),
            Panel::Terminal => {
                let (terminal, _) = self.terminals.get(self.active_terminal)?;
                terminal.focus_handle(cx)
            }
        })
    }

    /// The focus was in a panel that is gone. What its dock shows now
    /// takes it, or the file in front if the dock closed: left on what is
    /// no longer drawn, no key would reach anything.
    fn focus_dock(&mut self, place: Place, window: &mut Window, cx: &mut Context<Self>) {
        let shown = *self.dock(place);
        match (
            shown.and_then(|panel| self.panel_focus(panel, cx)),
            self.active_editor(),
        ) {
            (Some(handle), _) => window.focus(&handle),
            (None, Some(editor)) => window.focus(&editor.focus_handle(cx)),
            (None, None) => window.focus(&self.focus_handle),
        }
    }

    /// The keyboard is not left in a panel no dock shows: no key would
    /// reach anything from there. The file in front takes it.
    fn rescue_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let lost = Panel::ALL.into_iter().any(|panel| {
            !self.shown(panel)
                && self
                    .panel_focus(panel, cx)
                    .is_some_and(|focus| focus.contains_focused(window, cx))
        });
        if lost {
            match self.active_editor() {
                Some(editor) => window.focus(&editor.focus_handle(cx)),
                None => window.focus(&self.focus_handle),
            }
        }
    }

    /// Puts a panel's tab in the dock `to` by hand, before the tab of
    /// `before` or after the dock's others. In a dock it was not in, it
    /// is shown, and the dock it left goes on with what it has.
    fn put_panel(
        &mut self,
        panel: Panel,
        to: Place,
        before: Option<Panel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dragging = None;
        let from = Layout::get(cx).place(panel);
        let shown_in = Place::ALL
            .into_iter()
            .find(|place| *self.dock(*place) == Some(panel));
        if shown_in.is_some_and(|place| place != to) {
            self.panel_gone(panel, cx);
        }
        layout::put(Layout::get(cx).clone().moved(panel, to, before), cx);
        if from != Some(to) && self.available(panel) {
            self.show_panel(panel, cx);
        }
        self.rescue_focus(window, cx);
        cx.notify();
    }

    /// Takes a panel's tab out of the docks. Its command still opens it.
    fn hide_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown(panel) {
            self.panel_gone(panel, cx);
        }
        layout::put(Layout::get(cx).clone().hiding(panel), cx);
        self.rescue_focus(window, cx);
        cx.notify();
    }

    /// The window as it comes: the layout, and what the docks show.
    fn reset_layout(&mut self, _: &ResetLayout, window: &mut Window, cx: &mut Context<Self>) {
        self.dock_menu = None;
        self.bar_menu = None;
        layout::reset(cx);
        for place in Place::ALL {
            *self.dock(place) = Layout::get(cx).open_in(place);
        }
        self.rescue_focus(window, cx);
        self.tell_panels(None, cx);
    }

    /// A tab of a dock as the hand takes it: the right button opens the
    /// dock's menu on it, and it is dragged to another place, where it
    /// goes before the tab it is dropped on.
    fn held(
        &self,
        place: Place,
        panel: Panel,
        key: usize,
        tab: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (over, round) = (cx.theme().accent_soft, cx.theme().shape.control);
        let workspace = cx.weak_entity();
        div()
            .id(("dock-tab", key))
            .flex_none()
            .rounded(round)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.dock_menu = Some((event.position, place, Some(panel)));
                    this.bar_menu = None;
                    // The row of tabs would open its own, with no panel.
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_drag(DraggedPanel(panel), move |dragged, _, _, cx| {
                let panel = dragged.0;
                workspace
                    .update(cx, |this, cx| {
                        this.dragging = Some(panel);
                        cx.notify();
                    })
                    .ok();
                cx.new(|_| DraggedTab(panel))
            })
            .drag_over::<DraggedPanel>(move |style, dragged, _, _| {
                if dragged.0 == panel {
                    style
                } else {
                    style.bg(over)
                }
            })
            .on_drop(
                cx.listener(move |this, dragged: &DraggedPanel, window, cx| {
                    this.put_panel(dragged.0, place, Some(panel), window, cx)
                }),
            )
            .child(tab)
            .into_any_element()
    }

    /// Where a dragged tab is dropped to open a dock that is closed.
    fn drop_zone(&self, place: Place, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        div()
            .debug_selector(move || format!("drop-{}", place.id()))
            .flex_none()
            .bg(theme.bg_sunken)
            .border_color(theme.line)
            .map(|d| match place {
                Place::Left => d.w(px(36.)).h_full().border_r(theme.shape.border),
                Place::Right => d.w(px(36.)).h_full().border_l(theme.shape.border),
                Place::Bottom => d.h(px(36.)).w_full().border_t(theme.shape.border),
            })
            .drag_over::<DraggedPanel>(move |style, _, _, _| style.bg(theme.accent_soft))
            .on_drop(
                cx.listener(move |this, dragged: &DraggedPanel, window, cx| {
                    this.put_panel(dragged.0, place, None, window, cx)
                }),
            )
            .into_any_element()
    }

    fn menu_item(id: String, label: String, run: MenuRun, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let selector = id.clone();
        div()
            .id(SharedString::from(id))
            .debug_selector(move || selector.clone())
            .h(crate::theme::row(px(26.), cx))
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .rounded(theme.shape.token)
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .hover(|d| d.bg(theme.accent_soft))
            .child(label)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.dock_menu = None;
                this.bar_menu = None;
                this.editor_menu = None;
                run(this, window, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    fn menu_surface(
        position: Point<Pixels>,
        items: Vec<AnyElement>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        deferred(
            anchored().position(position).snap_to_window().child(
                div()
                    .id("layout-menu")
                    .occlude()
                    .w(crate::theme::text(240.))
                    .max_h(window.viewport_size().height)
                    .overflow_y_scroll()
                    .p_1()
                    .flex()
                    .flex_col()
                    .bg(theme.bg_elev)
                    .border(theme.shape.border)
                    .border_color(theme.line)
                    .rounded(theme.shape.control)
                    .shadow_lg()
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.dock_menu = None;
                        this.bar_menu = None;
                        this.editor_menu = None;
                        cx.notify();
                    }))
                    .children(items),
            ),
        )
    }

    /// What the conditions of extensions are read against in this window:
    /// the file in front, with what holds everywhere.
    fn extension_facts(&self, cx: &App) -> HashMap<String, serde_json::Value> {
        use serde_json::json;
        let mut facts = ExtensionStore::try_global(cx)
            .map(|store| store.read(cx).facts())
            .unwrap_or_default();
        let Some(editor) = self.active_editor().filter(|_| self.file_diff.is_none()) else {
            return facts;
        };
        let editor = editor.read(cx);
        let document = editor.doc(cx);
        let language = json!(crate::extension_api::language_id(document));
        let has_selection = editor.selections.iter().any(|s| s.anchor != s.head);
        for (name, value) in [
            ("editorLangId", language.clone()),
            ("resourceLangId", language),
            ("editorTextFocus", json!(true)),
            ("editorFocus", json!(true)),
            ("textInputFocus", json!(true)),
            ("editorIsOpen", json!(true)),
            ("editorHasSelection", json!(has_selection)),
            ("editorReadonly", json!(document.is_read_only())),
            ("resourceScheme", json!("file")),
        ] {
            facts.insert(name.into(), value);
        }
        if let Some(path) = document.path() {
            let part =
                |part: Option<&std::ffi::OsStr>| part.map(|p| p.to_string_lossy().into_owned());
            if let Some(name) = part(path.file_name()) {
                facts.insert("resourceFilename".into(), json!(name));
            }
            let ending = part(path.extension()).map(|ending| format!(".{ending}"));
            facts.insert("resourceExtname".into(), json!(ending.unwrap_or_default()));
        }
        facts
    }

    /// The right button in the file in front: what extensions put in the
    /// editor's menu for it. With nothing of theirs there is no menu.
    fn open_editor_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let facts = self.extension_facts(cx);
        let target = self
            .active_editor()
            .and_then(|editor| editor.read(cx).path(cx).map(Path::to_path_buf));
        let offered = ExtensionStore::try_global(cx)
            .map(|store| {
                store
                    .read(cx)
                    .menu("editor/context", &facts, target.as_deref())
            })
            .unwrap_or_default();
        if !offered.is_empty() {
            self.editor_menu = Some((position, offered));
            cx.notify();
        }
    }

    fn render_editor_menu(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (position, offered) = self.editor_menu.clone()?;
        let items = offered
            .into_iter()
            .enumerate()
            .map(|(i, offered)| {
                let action = offered.action;
                Self::menu_item(
                    format!("editor-menu-{i}"),
                    offered.title,
                    Box::new(move |_, window, cx| {
                        window.dispatch_action(Box::new(action.clone()), cx)
                    }),
                    cx,
                )
            })
            .collect();
        Some(Self::menu_surface(position, items, window, cx))
    }

    /// The menu of a dock: what can be done with the tab it was opened
    /// on, and the hidden panels, to bring one back into this dock.
    fn render_dock_menu(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (position, place, panel) = self.dock_menu?;
        let theme = cx.theme().clone();
        let layout = Layout::get(cx).clone();
        let item =
            |id, label, cx: &mut Context<Self>, run: MenuRun| Self::menu_item(id, label, run, cx);
        let separator = || {
            div()
                .my_1()
                .h(theme.shape.border)
                .bg(theme.line)
                .into_any_element()
        };
        let mut items: Vec<AnyElement> = Vec::new();
        if let Some(panel) = panel {
            if layout.place(panel).is_some() {
                items.push(item(
                    "menu-hide".into(),
                    format!("Hide {}", panel.label()),
                    cx,
                    Box::new(move |this, window, cx| this.hide_panel(panel, window, cx)),
                ));
            }
            for to in Place::ALL {
                if layout.place(panel) != Some(to) {
                    items.push(item(
                        format!("menu-move-{}", to.id()),
                        format!("Move to {} dock", to.id()),
                        cx,
                        Box::new(move |this, window, cx| {
                            this.put_panel(panel, to, None, window, cx)
                        }),
                    ));
                }
            }
            items.push(separator());
        }
        for hidden in layout.hidden.iter().copied() {
            items.push(item(
                format!("menu-show-{}", hidden.id()),
                format!("Show {}", hidden.label()),
                cx,
                Box::new(move |this, window, cx| this.put_panel(hidden, place, None, window, cx)),
            ));
        }
        if !layout.hidden.is_empty() {
            items.push(separator());
        }
        items.push(item(
            "menu-reset".into(),
            "Reset layout".into(),
            cx,
            Box::new(|this, window, cx| this.reset_layout(&ResetLayout, window, cx)),
        ));
        Some(Self::menu_surface(position, items, window, cx))
    }

    fn render_bar_menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (position, end, selected) = self.bar_menu.clone()?;
        let theme = cx.theme().clone();
        let layout = Layout::get(cx).clone();
        let separator = || {
            div()
                .my_1()
                .h(theme.shape.border)
                .bg(theme.line)
                .into_any_element()
        };
        let mut items = Vec::new();
        if let Some(item) = selected {
            let hidden = item.clone();
            items.push(Self::menu_item(
                "bar-menu-hide".into(),
                format!(
                    "Hide {}",
                    layout.style(&item).label.as_deref().unwrap_or(item.label())
                ),
                Box::new(move |_, _, cx| {
                    layout::put(Layout::get(cx).clone().hiding_item(hidden.clone()), cx);
                }),
                cx,
            ));
            for to in BarEnd::ALL {
                if layout.item_place(&item) != Some(to) {
                    let item = item.clone();
                    items.push(Self::menu_item(
                        format!("bar-menu-move-{}", to.id()),
                        format!("Move to {}", to.label()),
                        Box::new(move |_, _, cx| {
                            layout::put(
                                Layout::get(cx).clone().moved_item(item.clone(), to, None),
                                cx,
                            );
                        }),
                        cx,
                    ));
                }
            }
            items.push(separator());
            for (display, id, label) in [
                (Display::Text, "text", "Text"),
                (Display::Icon, "icon", "Icon"),
                (Display::Both, "both", "Text and icon"),
            ] {
                let item = item.clone();
                items.push(Self::menu_item(
                    format!("bar-menu-{id}"),
                    label.into(),
                    Box::new(move |_, _, cx| {
                        layout::put(Layout::get(cx).clone().styled(&item, display), cx);
                    }),
                    cx,
                ));
            }
            items.push(Self::menu_item(
                "bar-menu-edit".into(),
                "Edit icon or command".into(),
                Box::new(|this, window, cx| {
                    this.open_layout(&OpenLayout, window, cx);
                }),
                cx,
            ));
            if let Item::Button { button } = &item {
                let button = button.clone();
                items.push(Self::menu_item(
                    "bar-menu-remove".into(),
                    "Remove button".into(),
                    Box::new(move |_, _, cx| {
                        layout::put(Layout::get(cx).clone().without_button(&button), cx);
                    }),
                    cx,
                ));
            }
            items.push(separator());
        }
        for hidden in layout.named_items() {
            if layout.item_place(&hidden).is_none() {
                items.push(Self::menu_item(
                    format!("bar-menu-show-{}", hidden.id()),
                    format!(
                        "Show {}",
                        layout
                            .style(&hidden)
                            .label
                            .as_deref()
                            .unwrap_or(hidden.label())
                    ),
                    Box::new(move |_, _, cx| {
                        layout::put(
                            Layout::get(cx)
                                .clone()
                                .moved_item(hidden.clone(), end, None),
                            cx,
                        );
                    }),
                    cx,
                ));
            }
        }
        items.push(separator());
        items.push(Self::menu_item(
            "bar-menu-add".into(),
            "Add command button".into(),
            Box::new(move |this, window, cx| {
                let commands =
                    crate::bar_commands::Commands::new(end, this.plugins.clone(), window, cx);
                this.toggle_modal(window, cx, move |window, cx| {
                    Picker::new(commands, window, cx)
                });
            }),
            cx,
        ));
        items.push(Self::menu_item(
            "bar-menu-reset".into(),
            "Reset layout".into(),
            Box::new(|this, window, cx| this.reset_layout(&ResetLayout, window, cx)),
            cx,
        ));
        Some(Self::menu_surface(position, items, window, cx))
    }

    /// The dock a panel shows in.
    fn place_of(&self, panel: Panel, cx: &App) -> Place {
        Layout::get(cx).dock_of(panel)
    }

    fn dock(&mut self, place: Place) -> &mut Option<Panel> {
        match place {
            Place::Left => &mut self.left,
            Place::Right => &mut self.right,
            Place::Bottom => &mut self.bottom,
        }
    }

    /// Closes a dock. If the focus was in it, the file in front takes it,
    /// or with no file open the window itself: left on a field that is no
    /// longer drawn, no key would reach anything.
    fn close_dock(&mut self, place: Place, window: &mut Window, cx: &mut Context<Self>) {
        if self.dock(place).take().is_none() {
            return;
        }
        if self.dock_focus[place as usize].contains_focused(window, cx) {
            match self.active_editor() {
                Some(editor) => window.focus(&editor.focus_handle(cx)),
                None => window.focus(&self.focus_handle(cx)),
            }
        }
        self.docks_changed(None, cx);
    }

    /// Shows `panel` in its dock, opening the dock if it was closed.
    fn show_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        let place = self.place_of(panel, cx);
        *self.dock(place) = Some(panel);
        self.docks_changed(Some(panel), cx);
    }

    /// The left dock on a panel, or closed. A panel that lives in the
    /// other dock is shown there.
    fn set_sidebar(&mut self, tab: Option<Panel>, cx: &mut Context<Self>) {
        match tab {
            Some(panel) => self.show_panel(panel, cx),
            None => {
                self.left = None;
                self.docks_changed(None, cx);
            }
        }
    }

    /// The docks were changed by hand, or by what happened in the window:
    /// the panels are told, and the file is, so that the window starts
    /// the same way next time.
    fn docks_changed(&mut self, now: Option<Panel>, cx: &mut Context<Self>) {
        self.tell_panels(now, cx);
        let open = [self.left, self.right, self.bottom];
        layout::keep_open(open.into_iter().flatten().collect(), cx);
    }

    /// Tells the panels what the docks show now: the one that came into
    /// view reads what it shows, and services stop watching when unseen.
    fn tell_panels(&mut self, now: Option<Panel>, cx: &mut Context<Self>) {
        let visible = self.shown(Panel::ExtensionViews);
        self.extension_views
            .update(cx, |p, cx| p.set_visible(visible, cx));
        let visible = self.shown(Panel::Services);
        self.services.update(cx, |s, cx| s.set_visible(visible, cx));
        match now {
            Some(Panel::Database) => self.database_panel.update(cx, |p, cx| p.shown(cx)),
            Some(Panel::Api) => self.api_panel.update(cx, |p, cx| p.shown(cx)),
            Some(Panel::Ai) => self.ai_panel.update(cx, |p, cx| p.shown(cx)),
            Some(Panel::Extensions) => self.extensions_panel.update(cx, |p, cx| p.shown(cx)),
            Some(Panel::Chat) => self.chat.update(cx, |c, cx| c.shown(cx)),
            Some(Panel::Agent) => self.agent.update(cx, |a, cx| a.shown(cx)),
            _ => {}
        }
        cx.notify();
    }

    /// The layout changed. Where the file says which panels are open,
    /// those are: it is the truth, and it says so as soon as a dock was
    /// touched once. Until then a panel that is shown goes with its tab to
    /// the dock that got it, and a dock that lost what it showed, and got
    /// nothing in exchange, shows the first panel it has. The keyboard
    /// does not stay in a panel that is no longer shown: no key would
    /// reach anything from there.
    fn follow_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = Layout::get(cx).clone();
        let selection = layout::selection(cx);
        let switched = self.layout_selection != selection;
        self.layout_selection = selection;
        if switched {
            self.dock_menu = None;
            self.bar_menu = None;
        }
        let before = [self.left, self.right, self.bottom];
        for (index, place) in Place::ALL.into_iter().enumerate() {
            let now = if switched || layout.open.is_some() {
                layout.open_in(place).filter(|panel| self.available(*panel))
            } else {
                let here = before[index];
                // A hidden panel that is shown stays where it was opened.
                let stays = here.filter(|p| layout.place(*p).is_none_or(|now| now == place));
                let comes = before
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .find_map(|(_, shown)| shown.filter(|p| layout.place(*p) == Some(place)));
                let first = layout
                    .panels(place)
                    .iter()
                    .copied()
                    .find(|panel| self.available(*panel));
                stays.or(comes).or(here.and(first))
            };
            *self.dock(place) = now;
        }
        self.rescue_focus(window, cx);
        // A panel that came into view reads what it shows.
        for (index, place) in Place::ALL.into_iter().enumerate() {
            let now = *self.dock(place);
            if now != before[index] {
                self.tell_panels(now, cx);
            }
        }
        self.tell_panels(None, cx);
    }

    fn show_database(&mut self, _: &ShowDatabase, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(Panel::Database), cx);
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
        self.show_panel(Panel::Results, cx);
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
                        this.show_panel(Panel::Results, cx);
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

    /// The symbols of the file in front, to go to one.
    fn go_to_symbol(&mut self, _: &GoToSymbol, window: &mut Window, cx: &mut Context<Self>) {
        let Some(document) = self.active_editor().map(|e| e.read(cx).document().clone()) else {
            return;
        };
        let Some((path, source)) = crate::symbols::file_of(&document, cx) else {
            return;
        };
        let asked =
            LspStore::global(cx).and_then(|store| store.read(cx).document_symbols(&document));
        let workspace = cx.weak_entity();
        self.toggle_modal(window, cx, move |window, cx| {
            let picker = Picker::new(crate::symbols::Symbols::of_file(workspace), window, cx);
            crate::symbols::Symbols::load_file(asked, path, source, window, cx);
            picker
        });
    }

    /// The symbols of the project, as its language servers find them
    /// for the name being typed.
    fn go_to_project_symbol(
        &mut self,
        _: &GoToProjectSymbol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (workspace, root) = (cx.weak_entity(), self.root(cx));
        self.toggle_modal(window, cx, move |window, cx| {
            Picker::new(
                crate::symbols::Symbols::of_project(workspace, root),
                window,
                cx,
            )
        });
    }

    /// Lists the prompts of the context servers, to put one in the
    /// agent's field. The servers start for it as they do for a task.
    fn use_context_prompt(
        &mut self,
        _: &UseContextPrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.root(cx);
        let starting = crate::mcp_store::McpStore::global(cx)
            .update(cx, |store, cx| store.start_all(&root, cx));
        let workspace = cx.weak_entity();
        self.toggle_modal(window, cx, move |window, cx| {
            let picker = Picker::new(crate::context_prompts::Prompts::new(workspace), window, cx);
            crate::context_prompts::Prompts::load(starting, window, cx);
            picker
        });
    }

    /// Asks for what the chosen prompt still has to be told, one thing
    /// at a time from `asked` on; then the server writes the prompt and
    /// it goes into the agent's field, for the user to read and send.
    pub fn fill_prompt(
        &mut self,
        chosen: crate::mcp_store::ServerPrompt,
        told: Vec<(String, String)>,
        asked: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if asked < chosen.prompt.arguments.len() {
            let workspace = cx.weak_entity();
            self.toggle_modal(window, cx, move |window, cx| {
                let line = crate::context_prompts::Told::new(workspace, chosen, told, asked);
                Picker::new(line, window, cx)
            });
            return;
        }
        self.show_right(true, window, cx);
        let agent = self.agent.clone();
        let writing = cx
            .background_executor()
            .spawn(async move { chosen.text(&told) });
        cx.spawn(async move |_, cx| {
            let written = writing.await;
            agent
                .update(cx, |agent, cx| match written {
                    Ok(text) => {
                        let text = crate::context_prompts::one_line(&text);
                        agent
                            .input()
                            .update(cx, |input, cx| input.set_text(&text, false, cx));
                    }
                    Err(error) => agent.failed(error, cx),
                })
                .ok();
        })
        .detach();
    }

    /// Shows the chat or the agent, in the dock it is in, and focuses its
    /// field.
    fn show_right(&mut self, agent: bool, window: &mut Window, cx: &mut Context<Self>) {
        let panel = if agent { Panel::Agent } else { Panel::Chat };
        self.show_panel(panel, cx);
        let input = if agent {
            self.agent.read(cx).input()
        } else {
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
        // Its dock shows something else, or is closed: show the chat.
        if !self.shown(Panel::Chat) {
            return self.show_right(false, window, cx);
        }
        let input = self.chat.read(cx).input();
        if !input.focus_handle(cx).is_focused(window) {
            // Open but elsewhere: go to it rather than close it.
            window.focus(&input.focus_handle(cx));
            return;
        }
        let place = self.place_of(Panel::Chat, cx);
        self.close_dock(place, window, cx);
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
        self.set_sidebar(Some(Panel::Ai), cx);
        window.focus(&self.ai_panel.focus_handle(cx));
    }

    fn show_api(&mut self, _: &ShowApi, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(Panel::Api), cx);
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
        self.show_panel(Panel::Response, cx);
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
        self.show_panel(Panel::Debug, cx);
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

    fn show_extension_views(
        &mut self,
        _: &ShowExtensionViews,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_panel(Panel::ExtensionViews, cx);
        window.focus(&self.extension_views.focus_handle(cx));
    }

    fn show_extensions(&mut self, _: &ShowExtensions, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(Panel::Extensions), cx);
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
        let was_focused = self
            .debug_panel
            .focus_handle(cx)
            .contains_focused(window, cx);
        let place = self.place_of(Panel::Debug, cx);
        self.show_debug = false;
        self.panel_gone(Panel::Debug, cx);
        if was_focused {
            self.focus_dock(place, window, cx);
        }
        cx.notify();
    }

    fn render_debug_tab(
        &self,
        theme: &crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.shown(Panel::Debug);
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
            .rounded(theme.shape.control)
            .text_size(UI_FONT_SIZE)
            .text_color(if active { theme.fg } else { theme.fg_subtle })
            .when(active, |d| d.bg(theme.bg_elev))
            .hover(|d| d.text_color(theme.fg))
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_panel(Panel::Debug, cx);
            }))
            .when(paused, |d| {
                d.child(div().size(px(6.)).rounded(px(3.)).bg(theme.warning))
            })
            .child(crate::icons::draw(crate::icons::panel(Panel::Debug)))
            .child("Debug")
            .child(
                div()
                    .id("debug-close")
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.shape.token)
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
        let place = self.place_of(Panel::Response, cx);
        self.show_response = false;
        self.panel_gone(Panel::Response, cx);
        if was_focused {
            self.focus_dock(place, window, cx);
        }
        cx.notify();
    }

    /// Sends `request` and shows the Response tab; the keyboard stays where
    /// it was.
    pub fn send_request(&mut self, request: rest::Request, cx: &mut Context<Self>) {
        self.response.update(cx, |r, cx| r.send(request, cx));
        self.show_response = true;
        self.show_panel(Panel::Response, cx);
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
        let place = self.place_of(Panel::Results, cx);
        self.show_results = false;
        self.panel_gone(Panel::Results, cx);
        if was_focused {
            self.focus_dock(place, window, cx);
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
        self.extension_terminal_gone(terminal, cx);
        let was_focused = terminal.focus_handle(cx).contains_focused(window, cx);
        drop(self.terminals.remove(ix));
        if self.terminals.is_empty() {
            self.active_terminal = 0;
            self.panel_gone(Panel::Terminal, cx);
        } else {
            self.active_terminal = self.active_terminal.min(self.terminals.len() - 1);
        }
        if was_focused {
            let place = self.place_of(Panel::Terminal, cx);
            self.focus_dock(place, window, cx);
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
                if self.shown(Panel::Terminal)
                    && t.focus_handle(cx).contains_focused(window, cx) =>
            {
                // The dock the terminals are in, wherever that is.
                let place = self.place_of(Panel::Terminal, cx);
                self.close_dock(place, window, cx);
            }
            Some(t) => {
                self.show_panel(Panel::Terminal, cx);
                window.focus(&t.focus_handle(cx));
                cx.notify();
            }
        }
    }

    fn new_terminal(&mut self, _: &NewTerminal, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.default_terminal(cx);
        self.spawn_terminal(command, window, cx);
    }

    /// A tab for each terminal, and the button that opens another. They
    /// are in whichever dock the layout puts the terminals in.
    fn terminal_tabs(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let mut tabs: Vec<AnyElement> = self
            .terminals
            .iter()
            .enumerate()
            .map(|(ix, (terminal, _))| {
                let active = ix == self.active_terminal && self.shown(Panel::Terminal);
                let close = terminal.clone();
                div()
                    .id(("terminal-tab", ix))
                    .h(px(24.))
                    .pl_2p5()
                    .pr_1()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(theme.shape.control)
                    .text_size(UI_FONT_SIZE)
                    .text_color(if active { theme.fg } else { theme.fg_subtle })
                    .when(active, |d| d.bg(theme.bg_elev))
                    .hover(|d| d.text_color(theme.fg))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active_terminal = ix;
                        this.show_panel(Panel::Terminal, cx);
                        if let Some((t, _)) = this.terminals.get(ix) {
                            window.focus(&t.focus_handle(cx));
                        }
                        cx.notify();
                    }))
                    .child(crate::icons::draw(crate::icons::panel(Panel::Terminal)))
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
                            .rounded(theme.shape.token)
                            .hover(|d| d.bg(theme.line))
                            .child("×")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.remove_terminal(&close, window, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        tabs.push(
            div()
                .id("terminal-new")
                .debug_selector(|| "terminal-new".into())
                .flex_none()
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.shape.control)
                .text_color(theme.fg_subtle)
                .hover(|d| d.bg(theme.line).text_color(theme.fg))
                .child("+")
                .on_click(
                    cx.listener(|this, _, window, cx| this.new_terminal(&NewTerminal, window, cx)),
                )
                .into_any_element(),
        );
        tabs
    }

    fn render_results_tab(
        &self,
        theme: &crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.shown(Panel::Results);
        div()
            .id("results-tab")
            .debug_selector(|| "results-tab".into())
            .h(px(24.))
            .pl_2p5()
            .pr_1()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded(theme.shape.control)
            .text_size(UI_FONT_SIZE)
            .text_color(if active { theme.fg } else { theme.fg_subtle })
            .when(active, |d| d.bg(theme.bg_elev))
            .hover(|d| d.text_color(theme.fg))
            .on_click(cx.listener(|this, _, window, cx| {
                this.show_panel(Panel::Results, cx);
                window.focus(&this.results.focus_handle(cx));
                cx.notify();
            }))
            .child(crate::icons::draw(crate::icons::panel(Panel::Results)))
            .child("Results")
            .child(
                div()
                    .id("results-close")
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.shape.token)
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
        let active = self.shown(Panel::Response);
        div()
            .id("response-tab")
            .h(px(24.))
            .pl_2p5()
            .pr_1()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded(theme.shape.control)
            .text_size(UI_FONT_SIZE)
            .text_color(if active { theme.fg } else { theme.fg_subtle })
            .when(active, |d| d.bg(theme.bg_elev))
            .hover(|d| d.text_color(theme.fg))
            .on_click(cx.listener(|this, _, window, cx| {
                this.show_panel(Panel::Response, cx);
                window.focus(&this.response.focus_handle(cx));
                cx.notify();
            }))
            .child(crate::icons::draw(crate::icons::panel(Panel::Response)))
            .child("Response")
            .child(
                div()
                    .id("response-close")
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.shape.token)
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

    /// The borders that can be dragged: a strip over each, as long as the
    /// part it sizes is shown. A double click puts the size back.
    fn resize_handles(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        const GRIP: f32 = 6.;
        let layout = Layout::get(cx).clone();
        let dock = self.bottom.is_some();
        let top = layout.title_bar.height;
        let bottom = layout.status_bar.height + if dock { layout.bottom.height } else { 0. };
        let handle = |part: Part, cx: &mut Context<Self>| {
            div()
                .id(match part {
                    Part::Left => "resize-left",
                    Part::Right => "resize-right",
                    Part::Bottom => "resize-bottom",
                })
                .absolute()
                .occlude()
                .cursor(if part == Part::Bottom {
                    gpui::CursorStyle::ResizeUpDown
                } else {
                    gpui::CursorStyle::ResizeLeftRight
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        if event.click_count >= 2 {
                            this.resizing = None;
                            let standard = Layout::default().size(part);
                            cx.set_global(Layout::get(cx).clone().with(part, standard));
                            layout::keep(part, cx);
                            cx.refresh_windows();
                            return;
                        }
                        let at = match part {
                            Part::Bottom => event.position.y,
                            _ => event.position.x,
                        };
                        this.resizing = Some((part, f32::from(at), Layout::get(cx).size(part)));
                    }),
                )
        };
        let mut handles = Vec::new();
        if self.left.is_some() {
            handles.push(
                handle(Part::Left, cx)
                    .debug_selector(|| "resize-left".into())
                    .top(px(top))
                    .bottom(px(bottom))
                    .left(px(layout.left.width - GRIP / 2.))
                    .w(px(GRIP))
                    .into_any_element(),
            );
        }
        if self.right.is_some() {
            handles.push(
                handle(Part::Right, cx)
                    .debug_selector(|| "resize-right".into())
                    .top(px(top))
                    .bottom(px(bottom))
                    .right(px(layout.right.width - GRIP / 2.))
                    .w(px(GRIP))
                    .into_any_element(),
            );
        }
        if dock {
            handles.push(
                handle(Part::Bottom, cx)
                    .debug_selector(|| "resize-bottom".into())
                    .left_0()
                    .right_0()
                    .bottom(px(bottom - GRIP / 2.))
                    .h(px(GRIP))
                    .into_any_element(),
            );
        }
        handles
    }

    /// The pointer moved with a border held: the part follows it. The
    /// file is written once, when the border is let go.
    fn resize_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((part, from, size)) = self.resizing else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            return self.resize_end(cx);
        }
        // The sidebar grows to the right; the chat and the dock grow
        // towards the middle of the window, against the pointer's way.
        let moved = match part {
            Part::Left => f32::from(event.position.x) - from,
            Part::Right => from - f32::from(event.position.x),
            Part::Bottom => from - f32::from(event.position.y),
        };
        let layout = Layout::get(cx).clone().with(part, size + moved);
        if layout != *Layout::get(cx) {
            cx.set_global(layout);
            cx.refresh_windows();
        }
    }

    fn resize_end(&mut self, cx: &mut Context<Self>) {
        if let Some((part, _, size)) = self.resizing.take()
            && Layout::get(cx).size(part) != size
        {
            layout::keep(part, cx);
        }
    }

    fn open_layout(&mut self, _: &OpenLayout, window: &mut Window, cx: &mut Context<Self>) {
        let default = crate::layout::file(Layout::get(cx));
        self.open_config_file(crate::layout::path(cx), default, window, cx);
    }

    fn switch_layout(
        &mut self,
        action: &SwitchLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(name) = &action.name {
            let name = (name != layout::DEFAULT_NAME).then(|| name.clone());
            layout::choose(name, cx);
        } else {
            self.layout_picker(false, window, cx);
        }
    }

    fn save_layout(&mut self, _: &SaveLayout, window: &mut Window, cx: &mut Context<Self>) {
        self.layout_picker(true, window, cx);
    }

    fn switch_key_layout(
        &mut self,
        action: &SwitchKeyLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(name) = &action.name {
            crate::key_layout::choose(name.clone(), cx);
        } else {
            self.key_layout_picker(false, window, cx);
        }
    }

    fn save_key_layout(&mut self, _: &SaveKeyLayout, window: &mut Window, cx: &mut Context<Self>) {
        self.key_layout_picker(true, window, cx);
    }

    fn open_key_layout(&mut self, _: &OpenKeyLayout, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = crate::key_layout::path(cx) {
            self.open_path(path, None, window, cx);
        } else {
            // Built-in sets are copied first, so editing one cannot change
            // what Default or another editor's preset means.
            self.key_layout_picker(true, window, cx);
        }
    }

    fn key_layout_picker(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_modal(window, cx, move |window, cx| {
            let picker = Picker::new(KeyLayoutPicker::new(save), window, cx);
            KeyLayoutPicker::load(window, cx);
            picker
        });
    }

    fn layout_picker(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_modal(window, cx, move |window, cx| {
            let picker = Picker::new(LayoutPicker::new(save), window, cx);
            LayoutPicker::load(window, cx);
            picker
        });
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
        // A question of an extension that waited for the keyboard.
        cx.defer_in(window, |this, window, cx| this.extension_asks(window, cx));
        cx.notify();
    }

    /// What extensions asked of the window in front. Files to show and
    /// edits are done at once; a list to pick from or a line to type
    /// waits until nothing else has the keyboard, one at a time.
    fn extension_asks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::extension_api::Ask;
        let Some(store) = ExtensionStore::try_global(cx) else {
            return;
        };
        if !store.read(cx).is_front(&cx.entity()) {
            return;
        }
        loop {
            let free = self.modal.is_none();
            let Some(ask) = store.update(cx, |store, _| store.take_ask(free)) else {
                break;
            };
            match ask {
                Ask::Webview { key } => self.open_webview(key, window, cx),
                Ask::Pick {
                    title,
                    rows,
                    many,
                    reply,
                } => self.toggle_modal(window, cx, move |window, cx| {
                    let pick = crate::extension_ask::AskPick::new(title, rows, many, reply);
                    Picker::new(pick, window, cx)
                }),
                Ask::Input {
                    title,
                    value,
                    secret,
                    reply,
                } => self.toggle_modal(window, cx, move |window, cx| {
                    let input = crate::extension_ask::AskInput::new(title, reply);
                    let mut picker = Picker::new(input, window, cx);
                    if secret {
                        picker.mask(cx);
                    }
                    if !value.is_empty() {
                        picker.set_query(&value, cx);
                    }
                    picker
                }),
                Ask::Show { path, at, reply } => {
                    let jump = at.map(|range| Jump::Lsp {
                        range,
                        encoding: lsp::Encoding::Utf16,
                    });
                    self.open_path(path, jump, window, cx);
                    if let Some(reply) = reply {
                        reply.send(Ok(true.into()));
                    }
                }
                Ask::Edit { edit, reply } => {
                    self.apply_workspace_edit(edit, lsp::Encoding::Utf16, cx);
                    // After the documents said what changed: the extension
                    // reads the new text as soon as it has its answer.
                    cx.defer(move |_| reply.send(Ok(true.into())));
                }
                Ask::Save { path, reply } => {
                    let saving = self.save_for_extension(path.as_deref(), cx);
                    cx.spawn(async move |_, _| {
                        let saved = futures::future::join_all(saving).await;
                        reply.send(Ok(saved.into_iter().all(|saved| saved).into()));
                    })
                    .detach();
                }
                Ask::SaveAll => {
                    for saving in self.save_for_extension(None, cx) {
                        saving.detach();
                    }
                }
                Ask::Terminal {
                    extension,
                    terminal,
                    what,
                } => self.extension_terminal((extension, terminal), what, window, cx),
                Ask::Output { title, text } => {
                    let document = cx.new(|cx| {
                        Document::virtual_file(title, PathBuf::from("output.log"), &text, cx)
                    });
                    let editor = cx.new(|cx| Editor::for_document(document, cx));
                    self.add_tab(editor, window, cx);
                }
            }
        }
    }

    fn sync_webviews(&mut self, cx: &mut Context<Self>) {
        let Some(store) = ExtensionStore::try_global(cx) else {
            return;
        };
        let models = store.read(cx).api.webviews.clone();
        self.web_pages.retain(|(page, _)| {
            let key = page.read(cx).key.clone();
            if let Some(model) = models.get(&key) {
                page.update(cx, |page, cx| page.sync(model.clone(), cx));
                true
            } else {
                page.update(cx, |page, _| page.show(false));
                false
            }
        });
        if self
            .web_front
            .as_ref()
            .is_some_and(|(page, _)| !models.contains_key(&page.read(cx).key))
        {
            self.web_front = None;
        }
        for (page, _) in &self.web_pages {
            let key = page.read(cx).key.clone();
            let posts = store.update(cx, |store, _| store.api.web_posts.remove(&key));
            for (message, reply) in posts.into_iter().flatten() {
                let accepted = page.update(cx, |page, _| page.post(message));
                reply.send(Ok(accepted.into()));
            }
        }
        cx.notify();
    }

    fn open_webview(
        &mut self,
        key: crate::webview::Key,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = ExtensionStore::try_global(cx) else {
            return;
        };
        let Some(model) = store.read(cx).api.webviews.get(&key).cloned() else {
            return;
        };
        let page = self
            .web_pages
            .iter()
            .find(|(page, _)| page.read(cx).key == key)
            .map(|(page, _)| page.clone());
        let page = page.unwrap_or_else(|| {
            let page = cx.new(|cx| crate::webview::Page::new(key, model, cx));
            let events = cx.subscribe_in(&page, window, |this, _, event, window, cx| match event {
                crate::webview::PageEvent::Key(key) => match key.as_str() {
                    "close" => this.close_tab(&CloseTab, window, cx),
                    "commands" => {
                        window.focus(&this.focus_handle);
                        this.toggle_command_palette(&ToggleCommandPalette, window, cx);
                    }
                    "files" => {
                        window.focus(&this.focus_handle);
                        this.toggle_file_finder(&ToggleFileFinder, window, cx);
                    }
                    _ => {}
                },
            });
            self.web_pages.push((page.clone(), events));
            page
        });
        self.close_file_diff(window, cx);
        self.web_front = Some((page, self.active_pane));
        self.sync_webviews(cx);
    }

    fn close_webview(
        &mut self,
        page: &Entity<crate::webview::Page>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = page.read(cx).key.clone();
        page.update(cx, |page, _| page.show(false));
        self.web_front = None;
        self.web_pages.retain(|(known, _)| known != page);
        if let Some(store) = ExtensionStore::try_global(cx) {
            store.update(cx, |store, cx| store.close_webview(&key, cx));
        }
        if let Some(editor) = self.active_editor() {
            window.focus(&editor.focus_handle(cx));
        } else {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    fn webview_visibility(&mut self, cx: &mut Context<Self>) {
        let obscured = self.modal.is_some()
            || self.file_diff.is_some()
            || self.structure.is_some()
            || self.erd.is_some()
            || self.bar_menu.is_some()
            || self.dock_menu.is_some()
            || self.editor_menu.is_some()
            || self.dragging.is_some();
        for (page, _) in &self.web_pages {
            let selected = self
                .web_front
                .as_ref()
                .is_some_and(|(active, _)| active == page);
            page.update(cx, |page, _| page.show(selected && !obscured));
        }
    }

    fn webview_tabs(&self, pane: usize, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        self.web_pages
            .iter()
            .enumerate()
            .map(|(ix, (page, _))| {
                let selected = self
                    .web_front
                    .as_ref()
                    .is_some_and(|(active, _)| active == page);
                let title = page.read(cx).model.title.clone();
                let activate = page.clone();
                let close = page.clone();
                div()
                    .id(("web-tab", ix))
                    .debug_selector(move || format!("web-tab-{ix}"))
                    .flex_none()
                    .h(px(26.))
                    .max_w(px(260.))
                    .pl_3()
                    .pr_1p5()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(theme.shape.control)
                    .text_size(UI_FONT_SIZE)
                    .text_color(if selected { theme.fg } else { theme.fg_subtle })
                    .when(selected, |d| {
                        d.bg(theme.bg_elev)
                            .border(theme.shape.border)
                            .border_color(theme.line)
                    })
                    .hover(|d| d.text_color(theme.fg))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.web_front = Some((activate.clone(), pane));
                        // Keys go to the page that was chosen.
                        activate.read(cx).focus();
                        cx.notify();
                    }))
                    .child(crate::icons::draw("code"))
                    .child(div().min_w_0().truncate().child(title))
                    .child(
                        div()
                            .id(("web-close", ix))
                            .debug_selector(move || format!("web-close-{ix}"))
                            .flex_none()
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme.shape.token)
                            .hover(|d| d.bg(theme.line).text_color(theme.fg))
                            .child(crate::icons::draw("x"))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_webview(&close, window, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// What an extension does with a terminal of its own, in the dock of
    /// this window.
    fn extension_terminal(
        &mut self,
        key: (String, u64),
        what: crate::extension_api::TerminalAsk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::extension_api::TerminalAsk;
        let known = self
            .extension_terminals
            .iter()
            .find(|(of, _)| *of == key)
            .map(|(_, terminal)| terminal.clone());
        match (what, known) {
            (
                TerminalAsk::Create {
                    name,
                    program,
                    args,
                    cwd,
                    env,
                    keep,
                    show,
                },
                None,
            ) => {
                let command = TerminalCommand {
                    program,
                    args,
                    cwd: cwd.unwrap_or_else(|| self.root(cx)),
                    env,
                    title: name,
                    keep_on_exit: keep,
                };
                match self.spawn_terminal_with(command, show, window, cx) {
                    Some(terminal) => self.extension_terminals.push((key, terminal)),
                    // It could not be started: to the extension it ended.
                    None => {
                        if let Some(store) = ExtensionStore::try_global(cx) {
                            store.read(cx).terminal_closed(&key.0, key.1, None);
                        }
                    }
                }
            }
            (TerminalAsk::Send(text), Some(terminal)) => terminal.read(cx).write(text.into_bytes()),
            (TerminalAsk::Show, Some(terminal)) => {
                if let Some(ix) = self.terminals.iter().position(|(t, _)| *t == terminal) {
                    self.active_terminal = ix;
                }
                self.show_panel(Panel::Terminal, cx);
                cx.notify();
            }
            (TerminalAsk::Dispose, Some(terminal)) => self.remove_terminal(&terminal, window, cx),
            _ => {}
        }
    }

    /// A terminal ended or its tab was closed. If an extension made it,
    /// the extension hears of it, with what its program ended with.
    fn extension_terminal_gone(&mut self, terminal: &Entity<Terminal>, cx: &mut Context<Self>) {
        let made = self
            .extension_terminals
            .iter()
            .position(|(_, known)| known == terminal);
        let Some(ix) = made else {
            return;
        };
        let ((extension, id), terminal) = self.extension_terminals.remove(ix);
        let code = terminal.read(cx).exit_code;
        if let Some(store) = ExtensionStore::try_global(cx) {
            store.read(cx).terminal_closed(&extension, id, code);
        }
    }

    /// The tasks extensions have, to run one in a terminal.
    fn run_extension_task(
        &mut self,
        _: &RunExtensionTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = ExtensionStore::try_global(cx) else {
            return;
        };
        let listing = store.update(cx, |store, cx| store.tasks(cx));
        self.toggle_modal(window, cx, move |window, cx| {
            let picker = Picker::new(crate::extension_ask::TaskPick::default(), window, cx);
            crate::extension_ask::TaskPick::load(listing, window, cx);
            picker
        });
    }

    /// Saves the open file at `path`, or with none every open file that
    /// has changes, for an extension that asked.
    fn save_for_extension(
        &mut self,
        path: Option<&Path>,
        cx: &mut Context<Self>,
    ) -> Vec<Task<bool>> {
        let mut seen = HashSet::new();
        let editors: Vec<Entity<Editor>> = self
            .all_editors()
            .filter(|editor| {
                let document = editor.read(cx).document();
                let wanted = match (path, document.read(cx).path()) {
                    (Some(path), Some(own)) => path == own,
                    (None, Some(_)) => document.read(cx).is_dirty(),
                    _ => false,
                };
                wanted && seen.insert(document.entity_id())
            })
            .cloned()
            .collect();
        editors
            .into_iter()
            .map(|editor| editor.update(cx, |editor, cx| editor.save(cx)))
            .collect()
    }

    /// Runs a command of an extension's code. That it failed is said in
    /// the status bar; what it answers is the extension's own business.
    fn run_extension_command(
        &mut self,
        action: &crate::extension_api::RunExtensionCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = ExtensionStore::try_global(cx) else {
            return;
        };
        // A key bound under a condition that does not hold here is not
        // this command's: whoever else has the key gets it.
        if let Some(when) = &action.when {
            let facts = self.extension_facts(cx);
            if !extension::when::holds(when, &|name| facts.get(name).cloned()) {
                cx.propagate();
                return;
            }
        }
        let command = action.command.clone();
        let running = store.update(cx, |store, cx| {
            store.run_command(&command, action.args.clone(), cx)
        });
        cx.spawn(async move |_, cx| {
            if let Err(error) = running.await {
                store
                    .update(cx, |store, cx| {
                        store.report(format!("{command}: {error}"), cx)
                    })
                    .ok();
            }
        })
        .detach();
    }

    fn toggle_command_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let plugins = self.plugins.clone();
        let facts = self.extension_facts(cx);
        let extensions = ExtensionStore::try_global(cx)
            .map(|store| store.read(cx).palette(&facts))
            .unwrap_or_default();
        let palette = CommandPalette::new(plugins, extensions, window, cx);
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
        self.set_sidebar(Some(Panel::Files), cx);
        window.focus(&self.project_panel.focus_handle(cx));
        cx.notify();
    }

    fn show_search(&mut self, _: &ShowSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar(Some(Panel::Search), cx);
        let selected = self
            .active_editor()
            .and_then(|e| e.read(cx).selected_text(cx));
        self.project_search
            .update(cx, |s, cx| s.focus_query(selected, window, cx));
        cx.notify();
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, window: &mut Window, cx: &mut Context<Self>) {
        if self.left.is_some() {
            return self.close_dock(Place::Left, window, cx);
        }
        // Closed, it opens on the first panel it has.
        let first = Layout::get(cx)
            .left
            .panels
            .iter()
            .copied()
            .find(|panel| self.available(*panel));
        if let Some(panel) = first {
            self.left = Some(panel);
            self.docks_changed(Some(panel), cx);
        }
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
        self.set_sidebar(Some(Panel::Files), cx);
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
        let mut tabs: Vec<AnyElement> = pane
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let editor = tab.editor.clone();
                let doc = tab.editor.read(cx).doc(cx);
                let name: SharedString = doc.title().into();
                let icon = doc
                    .path()
                    .and_then(|path| crate::file_icons::file(path, cx));
                let dirty = doc.is_dirty();
                let active = pane.active == Some(ix) && self.web_front.is_none();
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
                    .rounded(theme.shape.control)
                    .text_size(UI_FONT_SIZE)
                    .text_color(if active && is_active_pane {
                        theme.fg
                    } else if active {
                        theme.fg_muted
                    } else {
                        theme.fg_subtle
                    })
                    .when(active, |d| {
                        d.bg(theme.bg_elev)
                            .border(theme.shape.border)
                            .border_color(theme.line)
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
                    .when_some(icon, |d, icon| {
                        d.child(
                            div()
                                .flex_none()
                                .debug_selector(move || format!("tab-icon-{ix}"))
                                .child(icon),
                        )
                    })
                    .child(name)
                    .child(
                        // Unsaved state doubles as the close target, like most editors.
                        div()
                            .id(("close", p * 10_000 + ix))
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme.shape.token)
                            .text_color(theme.fg_subtle)
                            .hover(|d| d.bg(theme.line).text_color(theme.fg))
                            .child(if dirty { "●" } else { "×" })
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close(&close_editor, window, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        if p == self.active_pane {
            tabs.extend(self.webview_tabs(p, cx));
        }
        let TabBar {
            height,
            place: tabs_at,
        } = Layout::get(cx).tab_bar;
        let mut tab_bar = (tabs_at != TabsAt::None).then(|| {
            div()
                .id(("tab-bar", p))
                .debug_selector(move || format!("tab-bar-{p}"))
                .h(px(height))
                .flex_none()
                .px_1p5()
                .flex()
                .items_center()
                .gap_1()
                .overflow_x_scroll()
                .map(|d| match tabs_at {
                    TabsAt::Bottom => d.border_t(theme.shape.border),
                    _ => d.border_b(theme.shape.border),
                })
                .border_color(theme.line)
                .bg(theme.bg_sunken)
                .children(tabs)
        });
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .when(p > 0, |d| {
                d.border_l(theme.shape.border).border_color(theme.line)
            })
            .children(if tabs_at == TabsAt::Top {
                tab_bar.take()
            } else {
                None
            })
            .when(search_visible, |d| d.child(self.search_bar.clone()))
            .when(is_active_pane, |pane| {
                pane.children(self.inline_edit.as_ref().map(|(panel, _)| panel.clone()))
            })
            .child(
                div()
                    .debug_selector(move || format!("pane-body-{p}"))
                    .flex_1()
                    .min_h_0()
                    .when(is_active_pane, |d| {
                        d.on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.open_editor_menu(event.position, cx)
                            }),
                        )
                    })
                    .map(|d| {
                        if let Some((page, page_pane)) = &self.web_front
                            && *page_pane == p
                        {
                            d.child(page.clone())
                        } else {
                            match pane.active_editor() {
                                Some(editor) => d.child(editor.clone()),
                                None => d.child(self.render_empty(window, cx)),
                            }
                        }
                    }),
            )
            // Below the file, if that is where the layout puts them.
            .children(tab_bar)
    }

    /// A dock: a tab for each panel the layout puts in it that has
    /// something to show, and the panel it shows.
    fn render_dock(&self, place: Place, tab: Panel, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let layout = Layout::get(cx).clone();
        // A hidden panel a command opened has a tab for as long as it shows.
        let mut panels = layout.panels(place).to_vec();
        if !panels.contains(&tab) {
            panels.push(tab);
        }
        let mut tabs: Vec<AnyElement> = Vec::new();
        for panel in panels {
            let key = panel as usize;
            match panel {
                // Each terminal's tab is the terminals' as far as the
                // layout goes: they move together.
                Panel::Terminal => {
                    for (ix, tab) in self.terminal_tabs(cx).into_iter().enumerate() {
                        tabs.push(self.held(place, panel, 100 + ix, tab, cx));
                    }
                }
                Panel::Debug if self.show_debug => {
                    let tab = self.render_debug_tab(&theme, cx).into_any_element();
                    tabs.push(self.held(place, panel, key, tab, cx));
                }
                Panel::Response if self.show_response => {
                    let tab = self.render_response_tab(&theme, cx).into_any_element();
                    tabs.push(self.held(place, panel, key, tab, cx));
                }
                Panel::Results if self.show_results => {
                    let tab = self.render_results_tab(&theme, cx).into_any_element();
                    tabs.push(self.held(place, panel, key, tab, cx));
                }
                Panel::Debug | Panel::Response | Panel::Results => {}
                _ => {
                    let active = tab == panel;
                    let label = div()
                        .id(("panel", panel as usize))
                        .debug_selector(move || format!("panel-{}", panel.id()))
                        .flex_none()
                        .h(px(24.))
                        .px_1()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .rounded(theme.shape.control)
                        .text_size(UI_FONT_SIZE)
                        .text_color(if active { theme.fg } else { theme.fg_subtle })
                        .when(active, |d| d.bg(theme.bg_elev))
                        .hover(|d| d.text_color(theme.fg))
                        .child(crate::icons::draw(crate::icons::panel(panel)))
                        .child(panel.label())
                        .on_click(cx.listener(move |this, _, window, cx| match panel {
                            Panel::Files => this.show_files(&ShowFiles, window, cx),
                            Panel::Search => this.show_search(&ShowSearch, window, cx),
                            Panel::Git => this.show_git(&ShowGit, window, cx),
                            Panel::Services => this.show_services(&ShowServices, window, cx),
                            Panel::Database => this.show_database(&ShowDatabase, window, cx),
                            Panel::Api => this.show_api(&ShowApi, window, cx),
                            Panel::Ai => this.show_ai(&ShowAi, window, cx),
                            Panel::Extensions => this.show_extensions(&ShowExtensions, window, cx),
                            Panel::ExtensionViews => {
                                this.show_extension_views(&ShowExtensionViews, window, cx)
                            }
                            Panel::Chat => this.show_right(false, window, cx),
                            Panel::Agent => this.show_right(true, window, cx),
                            _ => {}
                        }))
                        .into_any_element();
                    tabs.push(self.held(place, panel, key, label, cx));
                }
            }
        }
        div()
            .debug_selector(move || match place {
                Place::Left => "dock-left".into(),
                Place::Right => "dock-right".into(),
                Place::Bottom => "dock-bottom".into(),
            })
            .track_focus(&self.dock_focus[place as usize])
            .flex_none()
            .flex()
            .flex_col()
            .border_color(theme.line)
            // A tab from another dock dropped anywhere in this one comes
            // to the end of its row.
            .on_drop(
                cx.listener(move |this, dragged: &DraggedPanel, window, cx| {
                    if Layout::get(cx).place(dragged.0) != Some(place) {
                        this.put_panel(dragged.0, place, None, window, cx);
                    }
                }),
            )
            .map(|d| match place {
                Place::Left => d
                    .w(px(layout.left.width))
                    .h_full()
                    .border_r(theme.shape.border)
                    .bg(theme.bg_sunken),
                Place::Right => d
                    .w(px(layout.right.width))
                    .h_full()
                    .border_l(theme.shape.border)
                    .bg(theme.bg_sunken),
                Place::Bottom => d.h(px(layout.bottom.height)).border_t(theme.shape.border),
            })
            .child(
                div()
                    .flex_none()
                    .min_h(px(layout.tab_bar.height))
                    .px_2()
                    .flex()
                    // More tabs than the dock is wide go on a second row.
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .bg(theme.bg_sunken)
                    .border_b(theme.shape.border)
                    .border_color(theme.line)
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.dock_menu = Some((event.position, place, None));
                            this.bar_menu = None;
                            cx.notify();
                        }),
                    )
                    .on_drop(
                        cx.listener(move |this, dragged: &DraggedPanel, window, cx| {
                            this.put_panel(dragged.0, place, None, window, cx)
                        }),
                    )
                    .children(tabs),
            )
            .child(div().flex_1().min_h_0().map(|d| {
                match tab {
                    Panel::Files => d.pt_1().child(self.project_panel.clone()),
                    Panel::Search => d.pt_1().child(self.project_search.clone()),
                    Panel::Git => d.pt_1().child(self.git_panel.clone()),
                    Panel::Services => d.pt_1().child(self.services.clone()),
                    Panel::Database => d.pt_1().child(self.database_panel.clone()),
                    Panel::Api => d.pt_1().child(self.api_panel.clone()),
                    Panel::Ai => d.pt_1().child(self.ai_panel.clone()),
                    Panel::Extensions => d.pt_1().child(self.extensions_panel.clone()),
                    Panel::ExtensionViews => d.child(self.extension_views.clone()),
                    Panel::Chat => d.pt_1().child(self.chat.clone()),
                    Panel::Agent => d.pt_1().child(self.agent.clone()),
                    Panel::Debug => d.child(self.debug_panel.clone()),
                    Panel::Response => d.child(self.response.clone()),
                    Panel::Results => d.child(self.results.clone()),
                    Panel::Terminal => d.children(
                        self.terminals
                            .get(self.active_terminal)
                            .map(|(t, _)| t.clone()),
                    ),
                }
            }))
    }

    /// What is in front, as the window's title says it: the file, or the
    /// view that took its place, or with neither the project's folder.
    fn front_title(&self, cx: &App) -> String {
        if let Some((page, _)) = &self.web_front {
            return page.read(cx).model.title.clone();
        }
        self.structure
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
            .unwrap_or_else(|| self.root(cx).display().to_string())
    }

    /// What an item of a bar shows now, in as many pieces as it has to
    /// say; none while it has nothing to say.
    fn bar_item(&self, item: &Item, cx: &mut Context<Self>) -> Vec<BarPart> {
        let theme = cx.theme().clone();
        let says = |text: String| BarPart {
            text,
            id: None,
            color: None,
            action: None,
        };
        // The wrapper of the whole item keeps the window from being
        // dragged, while these parts keep their own click actions.
        let does = |id: (&'static str, usize),
                    text: String,
                    color: gpui::Hsla,
                    action: Box<dyn gpui::Action>| {
            BarPart {
                text,
                id: Some(id),
                color: Some(color),
                action: Some(action),
            }
        };
        // A file in front has a cursor, an indent and a language; a diff
        // in its place has none of them.
        let editor = match self.file_diff {
            Some(_) => None,
            None => self.active_editor(),
        };
        match item {
            Item::Project => {
                let root = self.root(cx);
                let name = root.file_name().map_or_else(
                    || root.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                vec![says(name)]
            }
            Item::File => {
                let title = self.front_title(cx);
                let root = self.root(cx);
                // Inside the project, the way from its folder is enough.
                let short = Path::new(&title)
                    .strip_prefix(&root)
                    .ok()
                    .filter(|rest| !rest.as_os_str().is_empty())
                    .map(|rest| rest.display().to_string());
                vec![says(short.unwrap_or(title))]
            }
            Item::Branch => {
                let branch = self.git.read(cx).status().branch.clone();
                branch
                    .map(|branch| {
                        does(
                            ("item-branch", 0),
                            branch,
                            theme.fg_muted,
                            Box::new(git_panel::SwitchBranch),
                        )
                    })
                    .into_iter()
                    .collect()
            }
            Item::Position => {
                if self.file_diff.is_some() {
                    return vec![says("Git diff".into()), says("Read-only".into())];
                }
                let Some(editor) = editor else {
                    return Vec::new();
                };
                let e = editor.read(cx);
                let (line, col, cursors) = e.cursor_position(cx);
                let mut parts = vec![says(format!("Ln {line}, Col {col}"))];
                if cursors > 1 {
                    parts.push(says(format!("{cursors} cursors")));
                }
                if e.doc(cx).is_read_only() {
                    parts.push(says("Read-only".into()));
                }
                parts
            }
            Item::Indent => editor
                .map(|e| says(e.read(cx).doc(cx).indent_label().to_string()))
                .into_iter()
                .collect(),
            Item::Language => editor
                .map(|e| {
                    let name = e.read(cx).doc(cx).language_name();
                    says(name.unwrap_or("Plain text").to_string())
                })
                .into_iter()
                .collect(),
            Item::Problems => editor
                .and_then(|e| {
                    let (errors, warnings) =
                        crate::editor_lsp::diagnostic_counts(e.read(cx).doc(cx));
                    crate::editor_lsp::status_text(errors, warnings)
                })
                .map(|text| says(text.to_string()))
                .into_iter()
                .collect(),
            Item::Activity => {
                let mut parts = Vec::new();
                if let Some(status) =
                    LspStore::global(cx).and_then(|s| s.read(cx).status().cloned())
                {
                    parts.push(says(status.to_string()));
                }
                if self.project.read(cx).is_scanning() {
                    parts.push(says("Indexing files...".into()));
                }
                // An extension's grammar that stopped answering: its
                // files are plain text until the editor starts again.
                for language in syntax::hung_grammars() {
                    parts.push(BarPart {
                        text: format!("{language} grammar hung: no highlighting until restart"),
                        id: None,
                        color: Some(theme.warning),
                        action: None,
                    });
                }
                parts
            }
            Item::Connection => {
                let connection = self.active_editor().and_then(|e| {
                    let path = e.read(cx).path(cx)?;
                    let store = self.database.read(cx);
                    match store.binding(path) {
                        Some(name) => Some(name.to_string()),
                        None => {
                            database::query_file_engines(path).map(|_| "No connection".to_string())
                        }
                    }
                });
                connection
                    .map(|name| {
                        does(
                            ("status-connection", 0),
                            name,
                            theme.fg_muted,
                            Box::new(SelectConnection),
                        )
                    })
                    .into_iter()
                    .collect()
            }
            // What the code of extensions shows. An item with a command
            // runs it.
            Item::Extensions => {
                let Some(store) = ExtensionStore::try_global(cx) else {
                    return Vec::new();
                };
                let bar = store.read(cx).bar();
                bar.into_iter()
                    .enumerate()
                    .map(|(i, item)| {
                        use crate::extension_api::{RunExtensionCommand, Tone};
                        let color = match item.tone {
                            Tone::Plain => theme.fg_muted,
                            Tone::Warning => theme.warning,
                            Tone::Error => theme.error,
                        };
                        let action = item.command.map(|(command, args)| {
                            let when = None;
                            Box::new(RunExtensionCommand {
                                command,
                                args,
                                when,
                            }) as Box<dyn gpui::Action>
                        });
                        BarPart {
                            text: item.text,
                            id: Some(("status-extension", i)),
                            color: Some(color),
                            action,
                        }
                    })
                    .collect()
            }
            // What plugins show, and a notice when one is slow; both open
            // the Plugins window.
            Item::Plugins => {
                let plugins = self.plugins.read(cx);
                let mut items: Vec<(String, bool)> = plugins
                    .status
                    .values()
                    .map(|text| (text.to_string(), false))
                    .collect();
                match plugins.slow().as_slice() {
                    [] => {}
                    [name] => items.push((format!("Slow plugin: {name}"), true)),
                    names => items.push((format!("{} slow plugins", names.len()), true)),
                }
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, (text, slow))| {
                        let color = if slow { theme.warning } else { theme.fg_muted };
                        does(("status-plugin", i), text, color, Box::new(ShowPlugins))
                    })
                    .collect()
            }
            Item::Performance => {
                let perf = cx.global::<Perf>();
                let mut parts = Vec::new();
                if perf.hud_visible {
                    if let Some((p50, p99)) = perf.input_p50_p99() {
                        parts.push(says(format!(
                            "input {} ms, p99 {}",
                            perf::ms(p50),
                            perf::ms(p99)
                        )));
                    }
                    if let Some((p50, _)) = perf.frame_p50_p99() {
                        parts.push(says(format!("frame {} ms", perf::ms(p50))));
                    }
                    if let Some(bytes) = perf::resident_memory() {
                        parts.push(says(format!("{} MB", bytes / (1024 * 1024))));
                    }
                    if let Some(start) = perf.first_frame {
                        parts.push(says(format!("start {} ms", perf::ms(start))));
                    }
                }
                parts
            }
            Item::Button { .. } => vec![says(
                Layout::get(cx)
                    .style(item)
                    .label
                    .as_deref()
                    .unwrap_or(item.label())
                    .to_owned(),
            )],
        }
    }

    /// Items are dropped before the one under the pointer. Even an empty
    /// end has room to append one or to bring one back through its menu.
    fn bar_end(&self, end: BarEnd, items: &[Item], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let children: Vec<_> = items
            .iter()
            .filter_map(|item| {
                let parts = self.bar_item(item, cx);
                if parts.is_empty() {
                    return None;
                }
                let style = Layout::get(cx).style(item).clone();
                let default_action = parts
                    .first()
                    .and_then(|part| part.action.as_ref())
                    .map(|action| action.boxed_clone());
                let tooltip = parts
                    .iter()
                    .map(|part| part.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ");
                let icon = style
                    .icon
                    .as_deref()
                    .and_then(crate::icons::named)
                    .unwrap_or_else(|| crate::icons::item(item));
                let whole = style.command.is_some()
                    || style.display == Display::Icon
                    || style.label.is_some();
                let content: Vec<_> = if style.display == Display::Icon {
                    Vec::new()
                } else if let Some(label) = style.label {
                    vec![div().child(label).into_any_element()]
                } else {
                    parts
                        .into_iter()
                        .map(|part| part.render(!whole, &theme))
                        .collect()
                };
                let id = item.id().to_owned();
                let menu_item = item.clone();
                let over_item = item.clone();
                let drop_item = item.clone();
                Some(
                    div()
                        .id(SharedString::from(format!("bar-item-{id}")))
                        .debug_selector(move || format!("item-{id}"))
                        .occlude()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.dock_menu = None;
                                this.bar_menu =
                                    Some((event.position, end, Some(menu_item.clone())));
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )
                        .on_drag(DraggedItem(item.clone()), |dragged, _, _, cx| {
                            cx.new(|_| DraggedBarItem(dragged.0.clone()))
                        })
                        .drag_over::<DraggedItem>(move |style, dragged, _, _| {
                            if dragged.0 == over_item {
                                style
                            } else {
                                style.bg(theme.accent_soft)
                            }
                        })
                        .on_drop(cx.listener(move |_, dragged: &DraggedItem, _, cx| {
                            layout::put(
                                Layout::get(cx).clone().moved_item(
                                    dragged.0.clone(),
                                    end,
                                    Some(drop_item.clone()),
                                ),
                                cx,
                            );
                        }))
                        .when(style.display != Display::Text, |d| {
                            d.child(crate::icons::draw(icon))
                        })
                        .child(div().flex().items_center().gap_4().children(content))
                        .when(whole, |d| {
                            d.hover(|d| d.bg(theme.line).text_color(theme.fg)).on_click(
                                move |_, window, cx| {
                                    if let Some(command) = &style.command {
                                        crate::bar_commands::run(command, window, cx);
                                    } else if let Some(action) = &default_action {
                                        window.dispatch_action(action.boxed_clone(), cx);
                                    }
                                },
                            )
                        })
                        .tooltip(move |_, cx| {
                            cx.new(|_| crate::ui::Tooltip(tooltip.clone())).into()
                        })
                        .into_any_element(),
                )
            })
            .collect();
        // Config errors stay visible even when their own layout is empty.
        let error = (end == BarEnd::StatusLeft)
            .then(|| {
                cx.try_global::<settings::ConfigErrors>()
                    .and_then(|e| e.0.first().cloned())
            })
            .flatten();
        div()
            .id(("bar-end", end as usize))
            .debug_selector(move || format!("bar-{}", end.id()))
            .flex_grow()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .gap_4()
            .when(end.is_right(), |d| d.justify_end())
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.dock_menu = None;
                    this.bar_menu = Some((event.position, end, None));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .drag_over::<DraggedItem>(move |style, _, _, _| style.bg(theme.accent_soft))
            .on_drop(cx.listener(move |_, dragged: &DraggedItem, _, cx| {
                layout::put(
                    Layout::get(cx)
                        .clone()
                        .moved_item(dragged.0.clone(), end, None),
                    cx,
                );
            }))
            .children(children)
            .children(error.map(|e| div().truncate().text_color(theme.error).child(e)))
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let bar = Layout::get(cx).status_bar.clone();
        div()
            .h(px(bar.height))
            .debug_selector(|| "status-bar".into())
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .border_t(theme.shape.border)
            .border_color(theme.line)
            .bg(theme.bg_sunken)
            .text_size(crate::theme::text(11.5))
            .text_color(theme.fg_subtle)
            .child(self.bar_end(BarEnd::StatusLeft, bar.left(), cx))
            .child(self.bar_end(BarEnd::StatusRight, bar.right(), cx))
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
        self.webview_visibility(cx);
        // A tab let go anywhere is no longer dragged.
        if self.dragging.is_some() && !cx.has_active_drag() {
            self.dragging = None;
        }
        // While one is, a closed dock has a place to drop it on.
        let zone = |this: &Self, place: Place, cx: &mut Context<Self>| {
            let closed = match place {
                Place::Left => this.left.is_none(),
                Place::Right => this.right.is_none(),
                Place::Bottom => this.bottom.is_none(),
            };
            (closed && this.dragging.is_some()).then(|| this.drop_zone(place, cx))
        };
        let title = self.front_title(cx);
        window.set_window_title(&title);
        let title_bar = Layout::get(cx).title_bar.clone();
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
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape"
                    && (this.bar_menu.is_some() || this.dock_menu.is_some())
                {
                    this.bar_menu = None;
                    this.dock_menu = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
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
            .on_action(cx.listener(Self::open_layout))
            .on_action(cx.listener(Self::switch_layout))
            .on_action(cx.listener(Self::run_extension_command))
            .on_action(cx.listener(Self::run_extension_task))
            .on_action(cx.listener(Self::save_layout))
            .on_action(cx.listener(Self::switch_key_layout))
            .on_action(cx.listener(Self::save_key_layout))
            .on_action(cx.listener(Self::open_key_layout))
            .on_action(cx.listener(Self::reset_layout))
            .on_action(cx.listener(Self::use_context_prompt))
            .on_action(cx.listener(Self::go_to_symbol))
            .on_action(cx.listener(Self::go_to_project_symbol))
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
            .on_action(cx.listener(Self::show_extension_views))
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
            .font_family(Settings::get(cx).ui_font())
            .text_color(theme.fg)
            .child(
                div()
                    .id("titlebar")
                    .debug_selector(|| "title-bar".into())
                    .h(px(title_bar.height))
                    .flex_none()
                    .flex()
                    .items_center()
                    // Room for the macOS traffic lights.
                    .pl(px(if cfg!(target_os = "macos") { 84. } else { 12. }))
                    .pr_3()
                    .border_b(theme.shape.border)
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
                    .child(self.bar_end(BarEnd::TitleLeft, title_bar.left(), cx))
                    .child(self.bar_end(BarEnd::TitleRight, title_bar.right(), cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .children(self.left.map(|tab| self.render_dock(Place::Left, tab, cx)))
                    .children(zone(self, Place::Left, cx))
                    .children(panes)
                    .children(zone(self, Place::Right, cx))
                    .children(
                        self.right
                            .map(|tab| self.render_dock(Place::Right, tab, cx)),
                    ),
            )
            .children(zone(self, Place::Bottom, cx))
            .children(
                self.bottom
                    .map(|tab| self.render_dock(Place::Bottom, tab, cx)),
            )
            .children(self.render_dock_menu(window, cx))
            .children(self.render_bar_menu(window, cx))
            .children(self.render_editor_menu(window, cx))
            .child(self.render_status(cx))
            .children(self.resize_handles(cx))
            .on_mouse_move(cx.listener(Self::resize_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.resize_end(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.resize_end(cx)),
            )
            .children(self.modal.as_ref().map(|modal| {
                deferred(
                    div()
                        .absolute()
                        .top(px(Layout::get(cx).title_bar.height + 24.))
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

    #[gpui::test]
    fn symbols_of_a_file_and_of_the_project_are_listed_to_go_to(cx: &mut TestAppContext) {
        use crate::symbols::Symbols;
        let root = fixture("lsp-symbols");
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        let file = root.join("src/main.rs");
        let other = root.join("src/lib.rs");
        std::fs::write(&file, "fn helper() {}\n\nfn main() {\n    helper();\n}\n").unwrap();
        std::fs::write(&other, "// a library\npub fn mainly() {}\n").unwrap();
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_lsp.py");
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        // What the list shows, and where the cursor of the file in front is.
        let shown = |cx: &mut VisualTestContext| -> Vec<(String, String, Option<String>)> {
            cx.read(|cx| {
                let modal = ws.read(cx).modal.as_ref()?;
                let picker = modal.view.clone().downcast::<Picker<Symbols>>().ok()?;
                Some(picker.read(cx).delegate.shown())
            })
            .unwrap_or_default()
        };
        let listed = |cx: &mut VisualTestContext, names: &[&str]| {
            for _ in 0..400 {
                cx.run_until_parked();
                let now: Vec<String> = shown(cx).into_iter().map(|row| row.0).collect();
                if now == names {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("the list never showed {names:?}, but {:?}", shown(cx));
        };
        let at = |cx: &mut VisualTestContext| {
            cx.read(|cx| {
                let editor = ws.read(cx).active_editor().unwrap().read(cx);
                let (line, column, _) = editor.cursor_position(cx);
                (editor.path(cx).map(Path::to_path_buf), line, column)
            })
        };

        // With no server that lists them (the language's is turned off
        // here), a file's symbols are the ones its outline finds.
        cx.update(|_, cx| {
            let mut settings = Settings::default();
            settings.language_servers.insert(
                "rust-analyzer".into(),
                settings::ServerOverride {
                    disabled: true,
                    ..Default::default()
                },
            );
            cx.set_global(settings);
        });
        ws.update_in(cx, |w, window, cx| {
            let content = std::fs::read_to_string(&file).unwrap();
            w.add_editor(Some(file.clone()), &content, None, window, cx)
        });
        cx.simulate_keystrokes("secondary-shift-o");
        listed(cx, &["helper", "main"]);
        assert_eq!(shown(cx)[0].1, "fn");
        cx.simulate_input("mai");
        listed(cx, &["main"]);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).modal.is_none()));
        assert_eq!(at(cx), (Some(file.clone()), 3, 1));

        // With a server, they are the server's, each with what it is in.
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
            w.open_path(other.clone(), None, window, cx)
        });
        let document = |cx: &App| {
            ws.read(cx)
                .active_editor()
                .unwrap()
                .read(cx)
                .document()
                .clone()
        };
        wait_for(cx, "the server", &|cx| {
            LspStore::global(cx)
                .is_some_and(|store| store.read(cx).document_symbols(&document(cx)).is_some())
        });
        cx.simulate_keystrokes("secondary-shift-o");
        listed(cx, &["crate", "mainly"]);
        assert_eq!(shown(cx)[1].1, "crate");
        cx.simulate_input("mainly");
        listed(cx, &["mainly"]);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        // The name itself is selected, not the line it is on.
        assert_eq!(at(cx), (Some(other.clone()), 2, 14));

        // The project's symbols are asked of the servers as the name is
        // typed, and say which file each is in. Going to one opens it.
        cx.simulate_keystrokes("secondary-t");
        cx.simulate_input("mainl");
        listed(cx, &["mainly"]);
        assert_eq!(shown(cx)[0].1, "crate  src/lib.rs:2");
        cx.simulate_keystrokes("escape");
        ws.update_in(cx, |w, window, cx| {
            w.open_path(file.clone(), None, window, cx)
        });
        wait_for(cx, "the first file at the server", &|cx| {
            LspStore::global(cx)
                .is_some_and(|store| store.read(cx).document_symbols(&document(cx)).is_some())
        });
        cx.simulate_keystrokes("secondary-t");
        cx.simulate_input("main");
        listed(cx, &["main", "mainly"]);
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        assert_eq!(at(cx), (Some(other), 2, 14));
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

    /// What a server draws into the text: hints in the lines, and colors
    /// over the grammar's. Against `tests/fixtures/mock_lsp.py --hints`.
    #[gpui::test]
    fn a_server_draws_hints_and_colors_into_the_text(cx: &mut TestAppContext) {
        use crate::document::Inlay;
        use syntax::HighlightKind;
        let root = fixture("lsp-hints");
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        let file = root.join("src/main.rs");
        std::fs::write(&file, "fn helper() {}\n// TODO fix\n").unwrap();
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_lsp.py");
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        let with = |hints: bool, colors: bool| {
            let mut settings = Settings::default();
            settings.language_servers.insert(
                "rust-analyzer".into(),
                settings::ServerOverride {
                    command: Some("python3".into()),
                    args: Some(vec![script.display().to_string(), "--hints".into()]),
                    ..Default::default()
                },
            );
            settings.inlay_hints = hints;
            settings.semantic_highlighting = colors;
            settings
        };
        cx.update(|_, cx| cx.set_global(with(true, true)));
        ws.update_in(cx, |w, window, cx| {
            let content = std::fs::read_to_string(&file).unwrap();
            w.add_editor(Some(file.clone()), &content, None, window, cx)
        });
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let drawn = |cx: &App| {
            let editor = editor.read(cx);
            let doc = editor.doc(cx);
            ((**doc.inlays()).clone(), (**doc.semantic()).clone())
        };
        let hint = |offset: usize| Inlay {
            offset,
            text: ": fn ".into(),
        };

        // Asked a moment after the file opened: a hint after the name of
        // the function, and the two words the server has a color for. The
        // third kind it names is none the editor knows, and is left out.
        wait_for(cx, "hints and colors", &|cx| {
            let (hints, colors) = drawn(cx);
            !hints.is_empty() && !colors.is_empty()
        });
        assert_eq!(
            cx.read(|cx| drawn(cx)),
            (
                vec![hint(9)],
                vec![
                    (3..9, HighlightKind::Function),
                    (18..22, HighlightKind::Function)
                ]
            )
        );
        // The hint is in the row and is no place in the file: the cursor
        // after `helper` stands before it, and `(` comes after it.
        cx.run_until_parked();
        let (before, after, inside) = cx.read(|cx| {
            let editor = editor.read(cx);
            let layout = editor.layout.as_ref().expect("the editor was drawn");
            let line = &layout.lines[0];
            let (before, after) = (line.x_for(9), line.x_for(10));
            let middle = gpui::point(
                layout.text_left + (before + after) / 2.,
                layout.bounds.top() + layout.line_height / 2.,
            );
            let buffer = editor.doc(cx).text();
            let inside = layout.offset_for_position(buffer, gpui::Point::default(), middle);
            (before, after, inside)
        });
        let em = cx.read(|cx| editor.read(cx).layout.as_ref().unwrap().em_width);
        // Five characters of hint and the `(` itself.
        assert!(
            (after - before - em * 6.).abs() < px(1.),
            "{before:?} {after:?} {em:?}"
        );
        assert_eq!(inside, 9);

        // Typed before them, they move with the text at once, and the
        // server's next answer says the same.
        let focus = cx.read(|cx| editor.focus_handle(cx));
        cx.update(|window, _| window.focus(&focus));
        cx.dispatch_action(crate::editor::MoveToStart);
        cx.simulate_input("x");
        assert_eq!(cx.read(|cx| drawn(cx).0), [hint(10)]);
        wait_for(cx, "the server's answer", &|cx| {
            drawn(cx).1
                == [
                    (4..10, HighlightKind::Function),
                    (19..23, HighlightKind::Function),
                ]
        });
        assert_eq!(cx.read(|cx| drawn(cx).0), [hint(10)]);
        // A second function gets a hint of its own.
        cx.dispatch_action(crate::editor::MoveToEnd);
        cx.simulate_input("fn two() {}");
        wait_for(cx, "the second hint", &|cx| drawn(cx).0.len() == 2);
        assert_eq!(cx.read(|cx| drawn(cx).0[1].offset), 34);

        // Each can be turned off, and is gone at the next change.
        cx.update(|_, cx| cx.set_global(with(false, true)));
        cx.simulate_input(" ");
        wait_for(cx, "the hints to go", &|cx| drawn(cx).0.is_empty());
        assert!(!cx.read(|cx| drawn(cx).1.is_empty()));
        cx.update(|_, cx| cx.set_global(with(false, false)));
        cx.simulate_input(" ");
        wait_for(cx, "the colors to go", &|cx| drawn(cx).1.is_empty());
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
        assert!(cx.read(|cx| ws.read(cx).bottom.is_none()));
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
        wait_for_with_timeout(cx, what, Duration::from_secs(5), f);
    }

    fn wait_for_with_timeout(
        cx: &mut VisualTestContext,
        what: &str,
        patience: Duration,
        f: &dyn Fn(&App) -> bool,
    ) {
        let deadline = Instant::now() + patience;
        while Instant::now() < deadline {
            // Debounce timers run on the test executor's virtual clock.
            cx.executor().advance_clock(Duration::from_millis(50));
            cx.run_until_parked();
            if cx.read(|cx| f(cx)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let lsp =
            cx.read(|cx| LspStore::global(cx).and_then(|store| store.read(cx).status().cloned()));
        let debugger = cx.read(|cx| {
            crate::debug::DebugStore::try_global(cx).map(|store| {
                let store = store.read(cx);
                (store.state.clone(), store.console.last().cloned())
            })
        });
        panic!(
            "timed out waiting for {what} after {patience:?}; \
             language server: {lsp:?}; debugger: {debugger:?}"
        );
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
        assert!(cx.read(|cx| ws.read(cx).left == Some(Panel::Services)));
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
            results.read(cx).query.contains("FROM \"users\"") && ws.read(cx).bottom.is_some()
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
            ws.show_panel(Panel::Results, cx);
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
        assert!(cx.read(|cx| ws.read(cx).left == Some(Panel::Database)));
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
            assert!(ws.show_results && ws.bottom == Some(Panel::Results));
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
        assert!(cx.read(|cx| ws.read(cx).bottom.is_none()));
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
        assert!(cx.read(|cx| ws.read(cx).bottom == Some(Panel::Response)));

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
        assert!(bounds.bottom() <= px(320. - 26.), "{bounds:?}");
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
    fn a_context_server_over_http_has_tools_resources_and_prompts(cx: &mut TestAppContext) {
        use crate::mcp_store::{McpStore, State};
        use std::io::BufRead;
        // The stand-in server, on a port of its own on this machine.
        struct Ended(std::process::Child);
        impl Drop for Ended {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../ai/tests/fixtures/mock_mcp_http.py");
        let mut mock = Ended(
            std::process::Command::new("python3")
                .arg(script)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut port = String::new();
        std::io::BufReader::new(mock.0.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        let url = format!("http://127.0.0.1:{}/mcp", port.trim());

        let root = db::testing::dir("ws-mcp-http").canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "plain\n").unwrap();
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        // In the settings it is an address and a key, not a command.
        let settings = serde_json::json!({ "context_servers": {
            "notes": { "url": url, "headers": { "Authorization": "Bearer t" } },
        } });
        cx.update(|_, cx| cx.set_global(settings::parse_settings(&settings.to_string()).unwrap()));

        // The list of prompts starts the servers, as a task does.
        cx.dispatch_action(UseContextPrompt);
        let store = cx.update(|_, cx| McpStore::global(cx));
        wait_for(cx, "the server", &|cx| {
            matches!(
                store.read(cx).servers.first().map(|entry| &entry.state),
                Some(State::Running(..) | State::Failed(_))
            )
        });
        let offer = cx.read(|cx| match &store.read(cx).servers[0].state {
            State::Running(_, offer) => offer.clone(),
            State::Failed(why) => panic!("{why}"),
            _ => unreachable!(),
        });
        assert_eq!(offer.tools.len(), 2);
        assert_eq!(offer.prompts.len(), 2);
        assert_eq!(offer.resources.len(), 2);

        // What it has to read is two more tools for the agent, next to
        // its own.
        let tools = cx.read(|cx| store.read(cx).tools());
        let names: Vec<&str> = tools.iter().map(|tool| tool.spec.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "mcp_notes_echo",
                "mcp_notes_slow",
                "mcp_notes_list_resources",
                "mcp_notes_read_resource"
            ]
        );
        let run = |name: &str, input: serde_json::Value| {
            tools
                .iter()
                .find(|tool| tool.spec.name == name)
                .unwrap()
                .run(input)
        };
        assert_eq!(
            run("mcp_notes_echo", serde_json::json!({ "text": "hi" })),
            ("hi".into(), false)
        );
        let (listed, failed) = run("mcp_notes_list_resources", serde_json::json!({}));
        assert!(!failed);
        assert_eq!(
            listed,
            "notes://today (Today): What is planned\nnotes://logo"
        );
        assert_eq!(
            run(
                "mcp_notes_read_resource",
                serde_json::json!({ "uri": "notes://today" })
            ),
            ("Ship the layout.".into(), false)
        );
        assert!(run("mcp_notes_read_resource", serde_json::json!({})).1);
        // The user is shown what such a call is before allowing it.
        let read = tools.iter().find(|tool| tool.tool == "read resource");
        assert!(
            read.unwrap()
                .shown(&serde_json::json!({ "uri": "notes://today" }))
                .starts_with("read resource of notes")
        );

        // A prompt is chosen from the list and told what it needs: one
        // thing that must be said, one that may be left out. What the
        // server writes goes into the agent's field, for the user to send.
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
        cx.simulate_input("review");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
        // Nothing typed for what must be said: the line stays.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
        cx.simulate_input("main");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
        cx.simulate_keystrokes("enter");
        let field = |cx: &App| ws.read(cx).agent.read(cx).input().read(cx).text(cx);
        wait_for(cx, "the prompt", &|cx| !field(cx).is_empty());
        assert_eq!(cx.read(|cx| field(cx)), "Review main. Be kind.");
        assert!(cx.read(|cx| ws.read(cx).modal.is_none()));
        assert_eq!(cx.read(|cx| ws.read(cx).right), Some(Panel::Agent));
    }

    #[gpui::test]
    fn the_agent_uses_a_context_servers_tools_after_asking(cx: &mut TestAppContext) {
        use crate::agent_task::{Entry, Status};
        // The model calls one tool of a context server twice, then another,
        // then proposes a plan.
        let (api, seen) = scripted_model(vec![
            tool_step("m1", "mcp_notes_echo", serde_json::json!({"text": "hello"})),
            tool_step("m2", "mcp_notes_echo", serde_json::json!({"text": "again"})),
            tool_step("m3", "mcp_notes_fail", serde_json::json!({})),
            tool_step(
                "p1",
                "update_plan",
                serde_json::json!({"steps": [{"text": "Nothing to do"}]}),
            ),
        ]);
        let repo = db::testing::dir("agent-mcp");
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
        std::fs::write(repo.join("notes.txt"), "plain\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let repo = repo.canonicalize().unwrap();
        let data = db::testing::dir("agent-mcp-data");
        let log = data.join("calls.jsonl");
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
        // Three servers in the settings: the stand-in, with a variable of
        // its own; one whose program is not there; one turned off.
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ai/tests/fixtures/mock_mcp.py");
        let settings = serde_json::json!({ "context_servers": {
            "notes": {
                "command": "python3",
                "args": [mock],
                "env": { "MOCK_MCP_LOG": log, "MOCK_MCP_KEY": "secret" },
            },
            "broken": { "command": "/no/such/server" },
            "off": { "command": "python3", "args": [mock], "enabled": false },
        } });
        cx.update(|_, cx| {
            cx.set_global(settings::parse_settings(&settings.to_string()).unwrap());
            crate::mcp_store::McpStore::global(cx)
                .update(cx, |mcp, _| mcp.env = Some(std::env::vars().collect()));
        });
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
        // No server runs before a task needs one.
        assert!(cx.read(|cx| {
            crate::mcp_store::McpStore::global_if_any(cx)
                .is_none_or(|mcp| mcp.read(cx).servers.is_empty())
        }));

        cx.simulate_keystrokes("secondary-shift-i");
        cx.simulate_input("Look at the notes");
        cx.simulate_keystrokes("enter");
        let panel = cx.read(|cx| ws.read(cx).agent.clone());
        let asked = |cx: &App, what: &str| {
            let task = panel.read(cx).task.as_ref()?.read(cx);
            match &task.status {
                Status::AwaitingApproval { command, .. } => Some(command.contains(what)),
                _ => None,
            }
        };
        // The first call of a tool waits for the user: it runs where the
        // server does, not in the sandbox.
        wait_for(cx, "the question about echo", &|cx| {
            asked(cx, "echo of notes") == Some(true)
        });
        let task = cx.read(|cx| panel.read(cx).task.clone().unwrap());
        assert!(!log.exists());
        // The model was offered the tools of the server that runs, next
        // to the agent's own, and none of the one turned off.
        let tools: Vec<String> = seen.lock().unwrap()[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        for name in [
            "read_file",
            "mcp_notes_echo",
            "mcp_notes_fail",
            "mcp_notes_data",
        ] {
            assert!(tools.contains(&name.to_string()), "{name}: {tools:?}");
        }
        assert!(!tools.iter().any(|name| name.starts_with("mcp_off")));
        // What the server says about itself is part of the instructions.
        let system = seen.lock().unwrap()[0]["messages"][0]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            system.contains("About notes: Ask before you look."),
            "{system}"
        );
        // The server that did not start is said, and the task went on.
        let notes = |cx: &App| -> Vec<String> {
            task.read(cx)
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Note(text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        };
        assert!(
            cx.read(|cx| notes(cx)).iter().any(|note| note.starts_with(
                "The context server broken did not start: Could not start /no/such/server"
            )),
            "{:?}",
            cx.read(|cx| notes(cx))
        );

        // Allowed, it runs, and the second call of the same tool does not
        // ask. Another tool does.
        task.update(cx, |task, cx| task.decide(true, cx));
        wait_for(cx, "the question about fail", &|cx| {
            asked(cx, "fail of notes") == Some(true)
        });
        let calls: Vec<serde_json::Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            calls,
            [
                serde_json::json!(["echo", {"text": "hello"}, "secret"]),
                serde_json::json!(["echo", {"text": "again"}, "secret"]),
            ]
        );
        let ran: Vec<(String, String)> = cx.read(|cx| {
            task.read(cx)
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Tool { title, output, .. } => Some((title.clone(), output.clone())),
                    _ => None,
                })
                .collect()
        });
        assert_eq!(
            ran[0],
            ("Called echo of notes".into(), "hello\n[image]".into())
        );
        assert_eq!(ran[1].1, "again\n[image]");

        // The AI tab lists the servers of the settings, each with how it
        // is doing.
        cx.dispatch_action(ShowAi);
        let ai_panel = cx.read(|cx| ws.read(cx).ai_panel.clone());
        ai_panel.update(cx, |panel, cx| {
            panel.show(crate::ai_panel::View::Servers, cx)
        });
        for row in ["context-server-0", "context-server-1", "context-server-2"] {
            bounds_soon(cx, row);
        }

        // Refused, it is not called, and the model is told.
        task.update(cx, |task, cx| task.decide(false, cx));
        wait_for(cx, "the plan", &|cx| {
            task.read(cx).status == Status::AwaitingPlan
        });
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
        let last = seen.lock().unwrap().last().unwrap().to_string();
        assert!(last.contains("The user declined to run"), "{last}");
        // The server stays for the next task; changing the settings stops
        // the ones that are no longer wanted.
        let running = |cx: &App| {
            crate::mcp_store::McpStore::global_if_any(cx)
                .unwrap()
                .read(cx)
                .servers
                .iter()
                .filter(|entry| matches!(entry.state, crate::mcp_store::State::Running(..)))
                .count()
        };
        assert_eq!(cx.read(|cx| running(cx)), 1);
        cx.update(|_, cx| {
            cx.set_global(Settings::default());
            let ready = crate::mcp_store::McpStore::global(cx)
                .update(cx, |mcp, cx| mcp.start_all(&repo, cx));
            drop(ready);
        });
        assert_eq!(cx.read(|cx| running(cx)), 0);
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
        assert_eq!(cx.read(|cx| ws.read(cx).bottom), Some(Panel::Debug));
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
            adapter: None,
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
    fn key_layouts_switch_copy_and_keep_personal_overrides(cx: &mut TestAppContext) {
        use crate::key_layout;
        let root = fixture("key-layouts");
        let config = db::testing::dir("ws-key-layouts");
        std::fs::write(
            config.join("settings.json"),
            "// mine\n{\"buffer_font_size\":16}\n",
        )
        .unwrap();
        std::fs::write(config.join(settings::IMPORTED_KEYMAP), r#"[{"context":"Editor && mode == full","bindings":{"ctrl-alt-d":"editor::SelectLine"}}]"#).unwrap();
        std::fs::create_dir_all(config.join("keymaps")).unwrap();
        let custom = config.join("keymaps/Review.json");
        std::fs::write(&custom, r#"[{"context":"Editor && mode == full","bindings":{"ctrl-alt-d":"editor::DuplicateLine"}}]"#).unwrap();
        std::fs::write(config.join("keymap.json"), r#"[{"context":"Workspace","bindings":{"alt-shift-r":["workspace::SwitchKeyLayout",{"name":"Review"}],"alt-shift-d":["workspace::SwitchKeyLayout",{"name":"Default"}]}}]"#).unwrap();
        cx.executor().allow_parking();
        let (ws, cx) = setup(cx, root.clone());
        cx.update(|_, cx| settings::reload_from(&config, cx));
        ws.update_in(cx, |w, window, cx| {
            w.add_editor(Some(root.join("keys.txt")), "one\ntwo", None, window, cx)
        });
        let wait = |cx: &mut VisualTestContext, name: &str| {
            wait_for(cx, "the selected keys", &|cx| {
                !key_layout::pending(cx) && key_layout::active(cx) == name
            });
            cx.run_until_parked();
        };
        cx.simulate_keystrokes("alt-shift-r");
        wait(cx, "Review");
        cx.simulate_keystrokes("ctrl-alt-d");
        assert_eq!(active_text(&ws, cx), "one\none\ntwo");
        // A full-editor binding leaves Enter to a picker's parent.
        cx.dispatch_action(SwitchKeyLayout::default());
        cx.simulate_input("VS Code");
        bounds_soon(cx, "key-layout-choice-0");
        cx.simulate_keystrokes("enter");
        wait(cx, key_layout::VSCODE);
        assert!(cx.read(|cx| ws.read(cx).modal.is_none()));
        cx.simulate_keystrokes("alt-shift-down");
        assert_eq!(active_text(&ws, cx), "one\none\none\ntwo");
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        editor.update(cx, |e, cx| e.select_range(0..4, cx));
        cx.simulate_keystrokes("backspace");
        assert_eq!(active_text(&ws, cx), "one\none\ntwo");
        cx.dispatch_action(SaveKeyLayout);
        cx.simulate_input("My keys");
        bounds_soon(cx, "key-layout-save");
        cx.simulate_keystrokes("enter");
        wait(cx, "My keys");
        let copied = std::fs::read_to_string(config.join("keymaps/My keys.json")).unwrap();
        assert_eq!(
            copied,
            import::keymap::to_json(&import::keymap::vscode_preset(cfg!(target_os = "macos")))
        );
        assert!(
            std::fs::read_to_string(config.join("settings.json"))
                .unwrap()
                .starts_with("// mine\n")
        );
        assert_eq!(cx.read(|cx| Settings::get(cx).buffer_font_size), 16.);

        // The user's own assignment wins over the selected file, which
        // in turn wins over the old imported assignment.
        cx.simulate_keystrokes("alt-shift-r");
        wait(cx, "Review");
        std::fs::write(config.join("keymap.json"), r#"[{"context":"Editor && mode == full","bindings":{"ctrl-alt-d":"editor::SelectLine"}}]"#).unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        cx.simulate_keystrokes("ctrl-alt-d backspace");
        assert_eq!(active_text(&ws, cx), "one\ntwo");
        // A partial invalid map is rejected as a whole. Its last good
        // keys remain active, and the file is left for the user to fix.
        std::fs::write(config.join("keymap.json"), "[]").unwrap();
        std::fs::write(
            &custom,
            r#"[{"bindings":{"ctrl-alt-d":"editor::SelectLine","ctrl-alt-x":"no::SuchAction"}}]"#,
        )
        .unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        wait_for(cx, "the rejected key file", &|cx| {
            !cx.global::<settings::ConfigErrors>().0.is_empty()
        });
        assert!(!cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));
        editor.update(cx, |e, cx| e.select_range(0..0, cx));
        cx.simulate_keystrokes("ctrl-alt-d");
        assert_eq!(active_text(&ws, cx), "one\none\ntwo");
        // Two immediate selections are serialized; the last one wins
        // both in the app and after reading the settings again.
        cx.dispatch_action(SwitchKeyLayout {
            name: Some(key_layout::JETBRAINS.into()),
        });
        cx.dispatch_action(SwitchKeyLayout {
            name: Some(key_layout::DEFAULT.into()),
        });
        wait(cx, key_layout::DEFAULT);
        cx.update(|_, cx| settings::reload_from(&config, cx));
        assert_eq!(
            cx.read(|cx| Settings::get(cx).key_layout.clone()),
            key_layout::DEFAULT
        );
        editor.update(cx, |e, cx| e.select_range(0..0, cx));
        cx.simulate_keystrokes("ctrl-alt-d backspace");
        assert_eq!(active_text(&ws, cx), "one\ntwo");
        let before = std::fs::read_to_string(config.join("settings.json")).unwrap();
        cx.dispatch_action(SwitchKeyLayout {
            name: Some("Missing".into()),
        });
        wait(cx, key_layout::DEFAULT);
        assert_eq!(
            std::fs::read_to_string(config.join("settings.json")).unwrap(),
            before
        );
        assert!(!cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));
        cx.dispatch_action(SwitchKeyLayout {
            name: Some("My keys".into()),
        });
        wait(cx, "My keys");
        assert!(cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));
        cx.dispatch_action(OpenKeyLayout);
        wait_for(cx, "the selected key file", &|cx| {
            ws.read(cx).active_editor().unwrap().read(cx).path(cx)
                == Some(config.join("keymaps/My keys.json").as_path())
        });
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

    use crate::extension_store::CodeState;
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
            r#"name = "Vue.js"
grammar = "vue"
path_suffixes = ["vue"]
code_fence_block_name = "vue"
block_comment = ["<!-- ", " -->"]
autoclose_before = ";:.,=}])>"
word_characters = ["-"]
increase_indent_pattern = ':\s*$'
decrease_indent_pattern = '^\s*end\b'
brackets = [
    { start = "{", end = "}", close = true, newline = true },
    { start = "<", end = ">", close = true, newline = true, not_in = ["string", "comment"] },
    { start = "\"", end = "\"", close = true, newline = false, not_in = ["string"] },
]
"#,
        );
        for query in [
            "highlights.scm",
            "injections.scm",
            "indents.scm",
            "brackets.scm",
            "overrides.scm",
            "outline.scm",
        ] {
            std::fs::copy(fixtures.join(query), dir.join("languages/vue").join(query)).unwrap();
        }
        std::fs::create_dir_all(dir.join("grammars")).unwrap();
        std::fs::copy(fixtures.join("vue.wasm"), dir.join("grammars/vue.wasm")).unwrap();
        write_file(&dir.join("themes/demo.json"), extension::testing::ZED_THEME);
        write_file(
            &dir.join("icon_themes/demo.json"),
            extension::testing::ZED_ICON_THEME,
        );
        for icon in ["file", "rust", "folder", "folder-open"] {
            write_file(
                &dir.join(format!("icons/{icon}.svg")),
                &extension::testing::svg(icon),
            );
        }
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
        click(cx, "panel-extensions");
        assert!(cx.read(|cx| ws.read(cx).left == Some(Panel::Extensions)));
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

        // And its icon theme gives files their pictures: the open file's
        // tab has one. The choice is a line in the settings.
        let icons = |cx: &App| {
            cx.try_global::<crate::file_icons::FileIcons>()
                .and_then(|icons| icons.0.as_ref().map(|theme| theme.name.clone()))
        };
        assert_eq!(cx.read(|cx| icons(cx)), None);
        click(cx, "extension-icons-0");
        wait_for(cx, "the icon theme", &|cx| {
            icons(cx).as_deref() == Some("Demo Icons")
        });
        assert_eq!(
            cx.read(|cx| cx.global::<Settings>().icon_theme.clone()),
            Some("Demo Icons".into())
        );
        bounds_soon(cx, "tab-icon-0");
        let rust = cx.read(|cx| crate::file_icons::file(Path::new("src/main.rs"), cx).is_some());
        assert!(rust);

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
        assert_eq!(cx.read(|cx| icons(cx)), None);
        assert!(cx.read(|cx| store.read(cx).find(Origin::Zed, "vue").is_some()));
        wait_for(cx, "the decision on disk", &|_| {
            std::fs::read_to_string(folder.join("state.json"))
                .is_ok_and(|text| text.contains("\"zed/vue\"") && text.contains("\"off\": true"))
        });
        click(cx, "extension-off");
        wait_for(cx, "the language to return", &|cx| {
            language(cx) == Some("Vue.js")
        });
        assert_eq!(cx.read(|cx| icons(cx)).as_deref(), Some("Demo Icons"));

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
    fn a_language_of_an_extension_is_typed_the_way_its_files_say(cx: &mut TestAppContext) {
        let _languages = extension_languages();
        let catalog = r#"{"data":[{"id":"vue","name":"Vue","version":"0.4.0","description":"Vue support.","download_count":1,"provides":["languages"]}]}"#;
        let (base, _) = serve(vec![
            ("/extensions", Served::ok(catalog.as_bytes().to_vec())),
            (
                "/extensions/vue/download",
                Served::ok(vue_archive("ext-typing")),
            ),
        ]);
        let (_config, store, ws, cx) = extension_setup(cx, "ext-typing", &base);
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        cx.dispatch_action(ShowExtensions);
        wait_for(cx, "the catalog", &|cx| {
            !store.read(cx).catalog(Origin::Zed).entries.is_empty()
        });
        store.update(cx, |store, cx| {
            let entry = store.catalog(Origin::Zed).entries[0].clone();
            store.install(entry, cx)
        });
        wait_for(cx, "the language", &|cx| {
            let doc = editor.read(cx).doc(cx);
            doc.language_name() == Some("Vue.js") && doc.syntax().is_some()
        });
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx)));

        // The whole file becomes `text`, with the cursor where `|` is.
        let put = |cx: &mut VisualTestContext, text: &str| {
            let at = text.find('|').unwrap();
            let text = text.replacen('|', "", 1);
            editor.update_in(cx, |e, _, cx| {
                let all = 0..e.text(cx).len();
                e.select_range(all, cx);
                e.insert(&text, cx);
                e.select_range(at..at, cx);
            });
            wait_for(cx, "the tree", &|cx| {
                editor
                    .read(cx)
                    .doc(cx)
                    .syntax()
                    .is_some_and(|s| !s.is_stale())
            });
        };
        // The text with `|` where the cursor is.
        let seen = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.read(|cx| {
                let editor = editor.read(cx);
                let mut text = editor.text(cx);
                text.insert(editor.newest_range().end, '|');
                text
            })
        };

        // Enter after a tag that opens: one level deeper, by `indents.scm`.
        put(
            cx,
            "<template>\n  <div class=\"main-nav\">|\n  </div>\n</template>\n",
        );
        cx.simulate_keystrokes("enter");
        assert_eq!(
            seen(cx),
            "<template>\n  <div class=\"main-nav\">\n    |\n  </div>\n</template>\n"
        );
        // `<` closes itself, as `config.toml` lists it, and its `>` is
        // typed over.
        cx.simulate_input("<");
        assert!(seen(cx).contains("    <|>\n"), "{}", seen(cx));
        cx.simulate_input("p>a");
        assert!(seen(cx).contains("    <p>a|\n"), "{}", seen(cx));
        // Enter after a line that closed its own tag: no deeper.
        cx.simulate_input("</p>");
        cx.simulate_keystrokes("enter");
        assert!(
            seen(cx).contains("    <p>a</p>\n    |\n  </div>"),
            "{}",
            seen(cx)
        );

        // A closing tag typed on a line of its own goes back under the tag
        // it closes.
        put(
            cx,
            "<template>\n  <div>\n    <p>a</p>\n    |\n</template>\n",
        );
        cx.simulate_input("</div>");
        assert_eq!(
            seen(cx),
            "<template>\n  <div>\n    <p>a</p>\n  </div>|\n</template>\n"
        );

        // The two patterns of `config.toml`: the line after one that ends
        // in a colon is deeper, and `end` goes back as it is typed. When
        // the word turns out to be another, the line returns.
        put(
            cx,
            "<template>\n  <div>\n    then:|\n  </div>\n</template>\n",
        );
        cx.simulate_keystrokes("enter");
        assert!(seen(cx).contains("    then:\n      |\n"), "{}", seen(cx));
        cx.simulate_input("en");
        assert!(seen(cx).contains("    then:\n      en|\n"), "{}", seen(cx));
        cx.simulate_input("d");
        assert!(seen(cx).contains("    then:\n    end|\n"), "{}", seen(cx));
        cx.simulate_input("less");
        assert!(
            seen(cx).contains("    then:\n      endless|\n"),
            "{}",
            seen(cx)
        );
        // A line the user moved stays where it was put while what is typed
        // changes nothing about it.
        put(
            cx,
            "<template>\n  <div>\n    <p>a</p>\n|\n  </div>\n</template>\n",
        );
        cx.simulate_input("text");
        assert!(seen(cx).contains("</p>\ntext|\n"), "{}", seen(cx));

        // A quote closes itself where a value starts, and not inside one:
        // `overrides.scm` says where a string is.
        put(cx, "<template>\n  <div class=|>\n  </div>\n</template>\n");
        cx.simulate_input("\"");
        assert!(seen(cx).contains("class=\"|\">"), "{}", seen(cx));
        put(
            cx,
            "<template>\n  <div class=\"a |b\">\n  </div>\n</template>\n",
        );
        cx.simulate_input("\"");
        assert!(seen(cx).contains("class=\"a \"|b\">"), "{}", seen(cx));
        // Backspace between the two halves of a pair takes both.
        put(cx, "<template>\n  <div>\n    |\n  </div>\n</template>\n");
        cx.simulate_input("{");
        assert!(seen(cx).contains("    {|}\n"), "{}", seen(cx));
        cx.simulate_keystrokes("backspace");
        assert!(seen(cx).contains("<div>\n    |\n"), "{}", seen(cx));

        // The language has no comment that runs to the end of a line: a
        // line goes between the two ends of the other kind, and back.
        put(
            cx,
            "<template>\n  <div>\n    <p>a|</p>\n  </div>\n</template>\n",
        );
        cx.simulate_keystrokes("secondary-/");
        assert!(
            seen(cx).contains("\n    <!-- <p>a|</p> -->\n"),
            "{}",
            seen(cx)
        );
        cx.simulate_keystrokes("secondary-/");
        assert!(seen(cx).contains("\n    <p>a|</p>\n"), "{}", seen(cx));
        // In the script it is the script's `//`, as before.
        put(
            cx,
            "<template></template>\n<script setup lang=\"ts\">\nconst a| = 1\n</script>\n",
        );
        cx.simulate_keystrokes("secondary-/");
        assert!(seen(cx).contains("\n// const a| = 1\n"), "{}", seen(cx));

        // `-` is part of a word here.
        put(
            cx,
            "<template>\n  <div class=\"|main-nav\">\n  </div>\n</template>\n",
        );
        cx.simulate_keystrokes("alt-right");
        assert!(seen(cx).contains("\"main-nav|\""), "{}", seen(cx));
    }

    #[gpui::test]
    fn a_pack_of_extensions_brings_what_it_is_made_of(cx: &mut TestAppContext) {
        // A pack: one extension that names another it is made of, and
        // says it needs a part of VS Code itself, which no catalog has.
        let dir = db::testing::dir("ws-ext-pack-archive");
        let package = |folder: &str, name: &str, more: &str| {
            let dir = dir.join(folder).join("extension");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("package.json"),
                format!(
                    r#"{{ "name": "{name}", "publisher": "Acme", "version": "1.0.0", "contributes": {{}}{more} }}"#
                ),
            )
            .unwrap();
        };
        package(
            "pack",
            "pack",
            r#", "extensionPack": ["Acme.member"], "extensionDependencies": ["vscode.git", "acme.member"]"#,
        );
        // The member needs the pack back: that ends where it began.
        package(
            "member",
            "member",
            r#", "extensionDependencies": ["Acme.pack"]"#,
        );
        let (Some(pack), Some(member)) = (
            zip(&dir.join("pack"), "extension"),
            zip(&dir.join("member"), "extension"),
        ) else {
            eprintln!("skipped: no python3 to build a .vsix");
            return;
        };
        let (files, _) = serve(vec![
            ("/pack.vsix", Served::ok(pack)),
            ("/member.vsix", Served::ok(member)),
        ]);
        let search = format!(
            r#"{{"extensions":[{{"namespace":"Acme","name":"pack","version":"1.0.0","files":{{"download":"{files}/pack.vsix"}}}}]}}"#
        );
        let found = format!(
            r#"{{"namespace":"Acme","name":"member","version":"1.0.0","files":{{"download":"{files}/member.vsix"}}}}"#
        );
        let (base, requests) = serve(vec![
            ("/api/-/search", Served::ok(search.into_bytes())),
            ("/api/Acme/member", Served::ok(found.into_bytes())),
            ("/extensions", Served::ok(br#"{"data":[]}"#.to_vec())),
        ]);
        let (_config, store, _ws, cx) = extension_setup(cx, "ext-pack", &base);
        cx.dispatch_action(ShowExtensions);
        wait_for(cx, "the catalog", &|cx| {
            !store.read(cx).catalog(Origin::VsCode).entries.is_empty()
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        // The pack is installed, and then what it is made of, with no
        // more asked of the user.
        let both = |cx: &App| {
            let store = store.read(cx);
            store.find(Origin::VsCode, "Acme.pack").is_some()
                && store.find(Origin::VsCode, "Acme.member").is_some()
        };
        for _ in 0..300 {
            cx.executor().advance_clock(Duration::from_millis(50));
            cx.run_until_parked();
            if cx.read(|cx| both(cx)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            cx.read(|cx| both(cx)),
            "installed {:?}, errors {:?}, asked {:?}",
            cx.read(|cx| store
                .read(cx)
                .installed
                .iter()
                .map(|e| e.id.clone())
                .collect::<Vec<_>>()),
            cx.read(|cx| store.read(cx).errors.values().cloned().collect::<Vec<_>>()),
            requests.lock().unwrap()
        );
        let pack = cx.read(|cx| store.read(cx).find(Origin::VsCode, "Acme.pack").cloned());
        assert_eq!(pack.unwrap().needs, ["Acme.member", "vscode.git"]);
        cx.run_until_parked();
        let asked = requests.lock().unwrap().clone();
        // The member was asked for once, for this machine, and the part
        // of VS Code and the pack itself not at all.
        let member = format!("/api/Acme/member/{}", extension::catalog::target());
        assert_eq!(
            asked.iter().filter(|r| **r == member).count(),
            1,
            "{asked:?}"
        );
        assert!(
            !asked
                .iter()
                .any(|r| r.contains("vscode") || r.contains("/api/Acme/pack"))
        );
    }

    #[gpui::test]
    fn a_vscode_extension_says_what_does_not_run_and_points_to_zed(cx: &mut TestAppContext) {
        // Its language becomes one of the editor's: the set of them is
        // one per process.
        let _languages = extension_languages();
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
        // A machine with no Node.js, and no way to one.
        store.update(cx, |store, _| {
            store.world = Some(std::sync::Arc::new(ServerOnPath("nothing", String::new())));
        });

        cx.dispatch_action(ShowExtensions);
        wait_for(cx, "the catalogs", &|cx| {
            let store = store.read(cx);
            !store.catalog(Origin::VsCode).entries.is_empty() && store.catalog(Origin::Zed).searched
        });
        cx.run_until_parked();
        // Enter installs what is selected. This one has code, which has
        // no sandbox: that is said before anything of it is in place.
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the download", &|cx| {
            let waiting = (Origin::VsCode, "vue.volar".to_string());
            store.read(cx).pending.contains_key(&waiting)
        });
        assert_eq!(
            cx.read(|cx| store.read(cx).asks(Origin::VsCode, "vue.volar")),
            ["Run its code with Node.js, outside a sandbox"]
        );
        assert!(cx.read(|cx| store.read(cx).find(Origin::VsCode, "vue.volar").is_none()));
        cx.run_until_parked();
        click(cx, "extension-allow");
        wait_for(cx, "the install", &|cx| {
            store.read(cx).find(Origin::VsCode, "vue.volar").is_some()
        });
        let installed = cx.read(|cx| store.read(cx).find(Origin::VsCode, "Vue.volar").cloned());
        let installed = installed.unwrap();
        assert_eq!(installed.themes.len(), 2);
        // Its code was allowed and waits for the start to be over, so it
        // is started; with no Node.js it says so, and the rest of the
        // extension works as before.
        assert!(installed.node().is_some());
        wait_for(cx, "its code to give up", &|cx| {
            let code = store.read(cx).code("Vue.volar");
            matches!(
                code.map(|code| &code.state),
                Some(CodeState::Stopped(why)) if why.contains("Node.js was not found")
            )
        });
        // Its language (files ending in .dm) is colored by the TextMate
        // grammar it brings, and typed as its configuration says.
        assert_eq!(installed.languages[0].suffixes, ["dm", "Demofile"]);
        let language = syntax::language_for_path(Path::new("notes.dm")).unwrap();
        assert_eq!(language.name, "Demo Lang");
        assert!(cx.read(|cx| ws.read(cx).left == Some(Panel::Extensions)));

        let notes = cx.read(|cx| ws.read(cx).root(cx)).join("notes.dm");
        std::fs::write(&notes, "if 1 # one\n").unwrap();
        ws.update_in(cx, |w, window, cx| {
            w.open_path(notes.clone(), None, window, cx)
        });
        let editor = |cx: &App| ws.read(cx).active_editor().unwrap().clone();
        wait_for(cx, "the colors of notes.dm", &|cx| {
            editor(cx).read(cx).path(cx) == Some(notes.as_path())
                && editor(cx).read(cx).doc(cx).syntax().is_some()
        });
        let colors = cx.read(|cx| {
            let editor = editor(cx);
            let doc = editor.read(cx).doc(cx);
            let rope = doc.text().rope();
            doc.syntax()
                .unwrap()
                .highlights(rope, 0..rope.len_bytes())
                .into_iter()
                .map(|(range, kind)| (rope.byte_slice(range).to_string(), kind))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            colors,
            [
                ("if".to_string(), syntax::HighlightKind::Keyword),
                ("1".to_string(), syntax::HighlightKind::Number),
                ("# one".to_string(), syntax::HighlightKind::Comment),
            ]
        );
        assert_eq!(
            cx.read(|cx| editor(cx).read(cx).doc(cx).language_name()),
            Some("Demo Lang")
        );
        // A brace closes itself, and Enter inside it goes a level in,
        // by the configuration's pairs and its two patterns.
        let focus = cx.read(|cx| editor(cx).focus_handle(cx));
        cx.update(|window, _| window.focus(&focus));
        cx.dispatch_action(crate::editor::MoveToEnd);
        cx.simulate_input("{");
        cx.run_until_parked();
        let text = |cx: &mut VisualTestContext| cx.read(|cx| editor(cx).read(cx).text(cx));
        assert_eq!(text(cx), "if 1 # one\n{}");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(text(cx), "if 1 # one\n{\n    \n}");
        cx.dispatch_action(ShowExtensions);
        cx.run_until_parked();

        // The settings it declares are what it says by default until
        // settings.json says otherwise, with the dots or as objects.
        let configured = |cx: &mut VisualTestContext, section: &str| {
            cx.read(|cx| store.read(cx).configuration(section, cx))
        };
        assert_eq!(
            configured(cx, "acme"),
            serde_json::json!({ "lint": { "level": 2 }, "format": true })
        );
        assert_eq!(configured(cx, "acme.lint.level"), 2);
        assert_eq!(configured(cx, "acme.name"), serde_json::Value::Null);
        cx.update(|_, cx| {
            let set = r#"{ "acme.lint.level": 3, "acme": { "format": false, "name": "x" } }"#;
            cx.set_global(settings::parse_settings(set).unwrap());
        });
        assert_eq!(
            configured(cx, "acme"),
            serde_json::json!({ "lint": { "level": 3 }, "format": false, "name": "x" })
        );
        assert_eq!(configured(cx, "")["acme"]["lint"]["level"], 3);
        cx.update(|_, cx| cx.set_global(Settings::default()));

        // Its icon theme is drawn with pictures, so it is one Solder has:
        // the tab offers it, and chosen, files get its pictures. The one
        // drawn with a font is among what does not work here.
        assert!(installed.missing.iter().any(|m| m.contains("icon theme")));
        let icons = |cx: &App| {
            cx.try_global::<crate::file_icons::FileIcons>()
                .and_then(|icons| icons.0.as_ref().map(|theme| theme.name.clone()))
        };
        assert_eq!(cx.read(|cx| icons(cx)), None);
        bounds_soon(cx, "extension-icons-0");
        // Chosen as its button does; the button is below what a window
        // of the test shows of so long a list.
        store.update(cx, |store, cx| {
            store.use_icon_theme("Acme Icons".into(), cx)
        });
        wait_for(cx, "the icon theme", &|cx| {
            icons(cx).as_deref() == Some("Acme Icons")
        });
        let picture = |cx: &mut VisualTestContext, name: &str| {
            cx.read(|cx| crate::file_icons::file(Path::new(name), cx).is_some())
        };
        assert!(picture(cx, "src/main.rs") && picture(cx, "Dockerfile"));
        assert!(cx.read(|cx| crate::file_icons::folder(Path::new("src"), true, cx).is_some()));

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

    /// This machine's Node.js and nothing else of the world.
    struct NodeOnly(String);

    impl extension::host::World for NodeOnly {
        fn node(&self) -> Result<String, String> {
            Ok(self.0.clone())
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
        fn which(&self, _: &str) -> Option<String> {
            None
        }
        fn env(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn status(&self, _: &str, _: extension::host::Status) {}
    }

    #[gpui::test]
    fn the_code_of_a_vscode_extension_runs_in_a_process_of_its_own(cx: &mut TestAppContext) {
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else {
            eprintln!("skipped: no node to run an extension with");
            return;
        };
        let _languages = extension_languages();
        let (base, _) = serve(vec![
            (
                "/api/-/search",
                Served::ok(br#"{"extensions":[]}"#.to_vec()),
            ),
            ("/extensions", Served::ok(br#"{"data":[]}"#.to_vec())),
        ]);
        let (_config, store, ws, cx) = extension_setup(cx, "ext-code", &base);
        let folder = cx.read(|cx| store.read(cx).root.clone());
        // Four extensions put in place by hand, so none was allowed: the
        // fixture, which has a command; one that asks for that command
        // when a Rust file is open; one that never returns from its
        // start; and one that ends its own process.
        extension::testing::vscode_extension(&folder.join("vscode/acme.demo"));
        let code = |name: &str, wakes: &str, main: &str| {
            let dir = folder.join(format!("vscode/acme.{name}"));
            write_file(
                &dir.join("package.json"),
                &format!(
                    r#"{{ "name": "{name}", "publisher": "Acme", "version": "1.0.0", "main": "main.js", "activationEvents": ["{wakes}"] }}"#
                ),
            );
            write_file(&dir.join("main.js"), main);
        };
        code(
            "other",
            "onLanguage:rust",
            r#"const vscode = require('vscode');
exports.activate = async () => {
  const answer = await vscode.commands.executeCommand('demo.run', 4);
  console.log('demo.run said ' + JSON.stringify(answer));
};"#,
        );
        code("spin", "*", "exports.activate = () => { for (;;) {} };");
        code(
            "quit",
            "onStartupFinished",
            r#"exports.activate = async (context) => {
  const starts = context.globalState.get('starts', 0) + 1;
  await context.globalState.update('starts', starts);
  console.log('start ' + starts);
  setTimeout(() => { process.stderr.write('out of luck\n'); process.exit(3); }, 200);
};"#,
        );
        store.update(cx, |store, cx| {
            store.world = Some(std::sync::Arc::new(NodeOnly(
                node.to_string_lossy().into_owned(),
            )));
            store.patience = Duration::from_secs(2);
            store.scan(cx);
        });
        wait_for(cx, "the four", &|cx| store.read(cx).installed.len() == 4);
        cx.run_until_parked();
        let state = |cx: &App, id: &str| store.read(cx).code(id).map(|code| code.state.clone());
        let said = |cx: &App, id: &str, what: &str| {
            store
                .read(cx)
                .code(id)
                .is_some_and(|code| code.said.iter().any(|(_, text)| text == what))
        };
        // Nothing of them runs: code has no sandbox, and nobody agreed.
        for id in ["Acme.demo", "Acme.other", "Acme.spin", "Acme.quit"] {
            assert_eq!(cx.read(|cx| state(cx, id)), None);
            assert_eq!(
                cx.read(|cx| store.read(cx).asks_installed(Origin::VsCode, id)),
                ["Run its code with Node.js, outside a sandbox"]
            );
        }

        // Allowed, the fixture is started: it waits for no more than the
        // editor to be up. Its commands are known, and what it asked for
        // that is not here.
        store.update(cx, |store, cx| store.allow(Origin::VsCode, "Acme.demo", cx));
        wait_for(cx, "the fixture's code", &|cx| {
            state(cx, "Acme.demo") == Some(CodeState::Running)
                && store.read(cx).code("Acme.demo").unwrap().commands.len() == 3
        });
        let demo = |cx: &App| {
            let code = store.read(cx).code("Acme.demo").unwrap();
            (code.commands.clone(), code.missing.clone())
        };
        assert_eq!(
            cx.read(|cx| demo(cx)),
            (
                vec!["demo.run".into(), "demo.spin".into(), "demo.quit".into()],
                vec!["notebooks.createNotebookController".to_string()]
            )
        );
        assert!(cx.read(|cx| said(cx, "Acme.demo", "demo started")));

        // The other is allowed in the tab. It waits for a Rust file, so
        // it is not started yet.
        cx.dispatch_action(ShowExtensions);
        cx.run_until_parked();
        cx.simulate_input("other");
        bounds_soon(cx, "extension-allow-code");
        click(cx, "extension-allow-code");
        assert!(cx.read(|cx| {
            store
                .read(cx)
                .asks_installed(Origin::VsCode, "Acme.other")
                .is_empty()
        }));
        assert_eq!(cx.read(|cx| state(cx, "Acme.other")), None);
        // A Rust file is opened: it starts, and asks for a command that is
        // the fixture's, which answers from its own process.
        let main = cx.read(|cx| ws.read(cx).root(cx)).join("main.rs");
        std::fs::write(&main, "fn main() {}\n").unwrap();
        ws.update_in(cx, |w, window, cx| w.open_path(main, None, window, cx));
        let answered = r#"demo.run said {"ran":[4],"starts":1}"#;
        wait_for(cx, "the other's code", &|cx| {
            said(cx, "Acme.other", answered)
        });
        assert_eq!(
            cx.read(|cx| state(cx, "Acme.other")),
            Some(CodeState::Running)
        );

        // One that never returns from its start is ended, and one that
        // ends itself is known to have, with its last words.
        store.update(cx, |store, cx| {
            store.allow(Origin::VsCode, "Acme.spin", cx);
            store.allow(Origin::VsCode, "Acme.quit", cx);
        });
        wait_for(cx, "the two to end", &|cx| {
            matches!(state(cx, "Acme.spin"), Some(CodeState::Stopped(_)))
                && matches!(state(cx, "Acme.quit"), Some(CodeState::Stopped(_)))
        });
        assert_eq!(
            cx.read(|cx| state(cx, "Acme.spin")),
            Some(CodeState::Stopped("It did not answer for 2 s".into()))
        );
        assert_eq!(
            cx.read(|cx| state(cx, "Acme.quit")),
            Some(CodeState::Stopped("out of luck".into()))
        );
        // Neither took the others along: the fixture still answers the
        // other, started again, and from the same process as before.
        assert_eq!(
            cx.read(|cx| state(cx, "Acme.demo")),
            Some(CodeState::Running)
        );
        store.update(cx, |store, cx| store.restart_code("Acme.other", cx));
        wait_for(cx, "the other's code again", &|cx| {
            said(cx, "Acme.other", answered)
        });
        // What ended is not started over and over, only when asked to.
        assert!(cx.read(|cx| said(cx, "Acme.quit", "start 1")));
        store.update(cx, |store, cx| {
            store.wake(cx);
            store.restart_code("Acme.quit", cx)
        });
        wait_for(cx, "the second start", &|cx| {
            said(cx, "Acme.quit", "start 2")
                && matches!(state(cx, "Acme.quit"), Some(CodeState::Stopped(_)))
        });

        // Turned off, its code is ended; removed, what it kept goes too.
        store.update(cx, |store, cx| {
            store.set_off(Origin::VsCode, "Acme.demo", true, cx)
        });
        assert_eq!(cx.read(|cx| state(cx, "Acme.demo")), None);
        assert!(folder.join("work/acme.quit/global.json").is_file());
        store.update(cx, |store, cx| {
            store.remove(Origin::VsCode, "Acme.quit", cx)
        });
        wait_for(cx, "the removal", &|cx| {
            store.read(cx).find(Origin::VsCode, "Acme.quit").is_none()
        });
        assert_eq!(cx.read(|cx| state(cx, "Acme.quit")), None);
        assert!(!folder.join("work/acme.quit").exists());
        // The host is Solder's own file, written once next to them.
        assert!(folder.join("host/host.js").is_file());
    }

    #[gpui::test]
    fn an_extension_reads_the_editor_and_asks_the_user(cx: &mut TestAppContext) {
        use crate::extension_api::{BarText, RunExtensionCommand, Tone};
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else {
            eprintln!("skipped: no node to run an extension with");
            return;
        };
        let (base, _) = serve(vec![
            (
                "/api/-/search",
                Served::ok(br#"{"extensions":[]}"#.to_vec()),
            ),
            ("/extensions", Served::ok(br#"{"data":[]}"#.to_vec())),
        ]);
        let (config, store, ws, cx) = extension_setup(cx, "ext-api", &base);
        let root = cx.read(|cx| ws.read(cx).root(cx));
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("other.rs"), "fn other() {}\n").unwrap();
        ws.update_in(cx, |w, window, cx| {
            w.open_path(root.join("main.rs"), None, window, cx)
        });
        let front = |cx: &App| {
            let editor = ws.read(cx).active_editor()?.read(cx);
            Some((editor.doc(cx).title(), editor.text(cx)))
        };
        wait_for(cx, "main.rs in front", &|cx| {
            front(cx).is_some_and(|(title, _)| title == "main.rs")
        });

        let folder = cx.read(|cx| store.read(cx).root.clone());
        let dir = folder.join("vscode/acme.api");
        write_file(
            &dir.join("package.json"),
            r#"{ "name": "api", "publisher": "Acme", "version": "1.0.0", "main": "main.js",
  "activationEvents": ["*"],
  "contributes": { "configuration": { "properties": { "api.level": { "type": "number", "default": 2 } } } } }"#,
        );
        write_file(
            &dir.join("main.js"),
            r#"const vscode = require('vscode');
exports.activate = async (context) => {
  const out = vscode.window.createOutputChannel('Api');
  const log = (...all) => out.appendLine(all.map((one) => (typeof one === 'string' ? one : JSON.stringify(one))).join(' '));
  const name = (uri) => vscode.workspace.asRelativePath(uri);
  log('folder', vscode.workspace.workspaceFolders[0].name);
  log('level', vscode.workspace.getConfiguration('api').get('level'), vscode.workspace.getConfiguration('editor').get('tabSize'));
  log('open', vscode.workspace.textDocuments.map((doc) => name(doc.uri)).sort());
  log('front', name(vscode.window.activeTextEditor.document.uri), vscode.window.activeTextEditor.document.languageId);
  vscode.workspace.onDidChangeTextDocument((e) => log('changed', e.contentChanges[0].text, e.document.lineAt(0).text));
  vscode.workspace.onDidChangeConfiguration((e) => {
    if (e.affectsConfiguration('api.level')) log('level now', vscode.workspace.getConfiguration('api').get('level'));
  });
  vscode.workspace.onDidOpenTextDocument((doc) => log('opened', name(doc.uri), doc.languageId));
  vscode.workspace.onDidSaveTextDocument((doc) => log('saved', name(doc.uri)));
  vscode.window.onDidChangeActiveTextEditor((e) => log('now front', e ? name(e.document.uri) : 'none'));
  vscode.window.onDidChangeTextEditorSelection((e) => log('cursor', e.selections[0].active.line, e.selections[0].active.character));
  const item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left);
  item.text = '$(rocket) Api ready';
  item.command = { command: 'api.ask', arguments: ['from the bar'] };
  item.show();
  context.subscriptions.push(vscode.commands.registerCommand('api.ask', async (from) => {
    const answer = await vscode.window.showInformationMessage('Go on?', 'Yes', 'No');
    const picked = await vscode.window.showQuickPick(['alpha', 'beta'], { placeHolder: 'Which one' });
    const typed = await vscode.window.showInputBox({ prompt: 'A name', value: 'abc' });
    log('asked', from, answer, picked, typed);
    const editor = vscode.window.activeTextEditor;
    await editor.edit((builder) => builder.insert(new vscode.Position(0, 0), '// hi\n'));
    log('after edit', editor.document.lineAt(0).text, editor.document.isDirty);
    await editor.document.save();
    await vscode.workspace.getConfiguration('api').update('level', 7);
    const shown = await vscode.window.showTextDocument(vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, 'other.rs'));
    log('shown', name(shown.document.uri));
    vscode.window.showErrorMessage('It broke');
    out.show();
  }));
  log('ready');
};"#,
        );
        store.update(cx, |store, cx| {
            store.world = Some(std::sync::Arc::new(NodeOnly(
                node.to_string_lossy().into_owned(),
            )));
            store.scan(cx);
        });
        wait_for(cx, "the extension", &|cx| {
            store.read(cx).find(Origin::VsCode, "Acme.api").is_some()
        });
        store.update(cx, |store, cx| store.allow(Origin::VsCode, "Acme.api", cx));
        let wrote = |cx: &App, line: &str| {
            let store = store.read(cx);
            let output = store.output("Acme.api", "Api").unwrap_or_default();
            output.lines().any(|known| known == line)
        };
        let written = |cx: &mut VisualTestContext, line: &'static str| {
            for _ in 0..500 {
                cx.executor().advance_clock(Duration::from_millis(50));
                cx.run_until_parked();
                if cx.read(|cx| wrote(cx, line)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = cx.read(|cx| {
                let output = store.read(cx).output("Acme.api", "Api");
                output.unwrap_or_default().to_string()
            });
            panic!("no line {line:?} in what it wrote:\n{output}");
        };

        // Loaded, it already has the folder, the settings with what it
        // declares, the files that are open and the one in front.
        written(cx, "ready");
        let name = root.file_name().unwrap().to_string_lossy().into_owned();
        let output = cx.read(|cx| {
            store
                .read(cx)
                .output("Acme.api", "Api")
                .unwrap()
                .to_string()
        });
        assert_eq!(
            output.lines().collect::<Vec<_>>(),
            [
                format!("folder {name}").as_str(),
                "level 2 4",
                r#"open ["App.vue","main.rs"]"#,
                "front main.rs rust",
                "ready",
            ]
        );
        // Its item is in the status bar, without the picture VS Code
        // would draw, and a click runs its command with what goes with it.
        let (command, args) = ("api.ask".to_string(), serde_json::json!(["from the bar"]));
        assert_eq!(
            cx.read(|cx| store.read(cx).bar()),
            [BarText {
                text: "Api ready".into(),
                tone: Tone::Plain,
                command: Some((command.clone(), args.clone())),
            }]
        );
        let when = None;
        cx.dispatch_action(RunExtensionCommand {
            command,
            args,
            when,
        });

        // It asks three things, one after another, each in the editor's
        // own list: a message with answers, one of a list, a line to type.
        let asking = |cx: &App| ws.read(cx).modal.is_some();
        wait_for(cx, "the message", &asking);
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the list", &asking);
        cx.simulate_input("bet");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the line to type", &asking);
        let guidance = bounds_soon(cx, "extension-input-guidance");
        let list = bounds_soon(cx, "picker-matches-viewport");
        assert!(guidance.top() >= list.top());
        assert!(guidance.bottom() <= list.bottom());
        // What it suggested is selected, so typing replaces it.
        cx.simulate_input("xyz");
        cx.simulate_keystrokes("enter");
        written(cx, "asked from the bar Yes beta xyz");

        // It changes the file in front, reads its own change, saves the
        // file, sets a setting and hears of it, and brings another file to
        // the front.
        written(cx, "changed // hi");
        written(cx, "after edit // hi true");
        written(cx, "saved main.rs");
        assert_eq!(
            std::fs::read_to_string(root.join("main.rs")).unwrap(),
            "// hi\nfn main() {}\n"
        );
        written(cx, "level now 7");
        let settings = std::fs::read_to_string(config.join("settings.json")).unwrap();
        assert!(settings.contains(r#""api.level": 7"#), "{settings}");
        written(cx, "opened other.rs rust");
        written(cx, "now front other.rs");
        written(cx, "shown other.rs");
        // A message that asks nothing is in the status bar, and what it
        // wrote opens in a tab when it says so.
        wait_for(cx, "its output", &|cx| {
            front(cx).is_some_and(|(title, text)| {
                title == "Output: Api" && text.contains("shown other.rs")
            })
        });
        let bar = cx.read(|cx| store.read(cx).bar());
        assert_eq!(
            bar.last(),
            Some(&BarText {
                text: "It broke".into(),
                tone: Tone::Error,
                command: None,
            })
        );
        // The message goes after a while; the item stays.
        cx.executor().advance_clock(Duration::from_secs(9));
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| store.read(cx).bar().len()), 1);

        // What is typed reaches it as it is typed, with where the cursor is.
        ws.update_in(cx, |w, window, cx| {
            w.open_path(root.join("other.rs"), None, window, cx)
        });
        written(cx, "now front other.rs");
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let focus = cx.read(|cx| editor.focus_handle(cx));
        cx.update(|window, _| window.focus(&focus));
        cx.simulate_input("x");
        written(cx, "changed x xfn other() {}");
        written(cx, "cursor 0 1");

        // Turned off, its code is ended and its item leaves the bar.
        store.update(cx, |store, cx| {
            store.set_off(Origin::VsCode, "Acme.api", true, cx)
        });
        assert!(cx.read(|cx| store.read(cx).bar().is_empty()));
    }

    #[gpui::test]
    fn a_list_of_an_extension_takes_several_and_a_password_is_not_shown(cx: &mut TestAppContext) {
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else {
            eprintln!("skipped: no node to run an extension with");
            return;
        };
        let (base, _) = serve(vec![
            (
                "/api/-/search",
                Served::ok(br#"{"extensions":[]}"#.to_vec()),
            ),
            ("/extensions", Served::ok(br#"{"data":[]}"#.to_vec())),
        ]);
        let (_config, store, ws, cx) = extension_setup(cx, "ext-many", &base);
        let folder = cx.read(|cx| store.read(cx).root.clone());
        let dir = folder.join("vscode/acme.many");
        write_file(
            &dir.join("package.json"),
            r#"{ "name": "many", "publisher": "Acme", "version": "1.0.0", "main": "main.js",
  "activationEvents": ["onCommand:many.ask"] }"#,
        );
        write_file(
            &dir.join("main.js"),
            r#"const vscode = require('vscode');
exports.activate = (context) => {
  context.subscriptions.push(vscode.commands.registerCommand('many.ask', async () => {
    const picked = await vscode.window.showQuickPick(
      [{ label: 'alpha' }, { label: 'beta', picked: true }, { label: 'gamma' }],
      { canPickMany: true, placeHolder: 'Which ones' });
    const none = await vscode.window.showQuickPick(['x', 'y'], { canPickMany: true });
    const secret = await vscode.window.showInputBox({ prompt: 'Password', password: true });
    console.log(`picked ${picked.map((item) => item.label)}; none ${JSON.stringify(none)}; secret ${secret}`);
  }));
};"#,
        );
        store.update(cx, |store, cx| {
            store.world = Some(std::sync::Arc::new(NodeOnly(
                node.to_string_lossy().into_owned(),
            )));
            store.scan(cx);
        });
        wait_for(cx, "the extension", &|cx| {
            store.read(cx).find(Origin::VsCode, "Acme.many").is_some()
        });
        store.update(cx, |store, cx| {
            store.allow(Origin::VsCode, "Acme.many", cx);
            store
                .run_command("many.ask", serde_json::Value::Null, cx)
                .detach()
        });
        let list = |cx: &App| {
            let modal = ws.read(cx).modal.as_ref()?;
            let picker = modal.view.clone();
            picker
                .downcast::<Picker<crate::extension_ask::AskPick>>()
                .ok()
        };
        let picked = |cx: &App| -> Option<Vec<String>> {
            let list = list(cx)?;
            let picked = list.read(cx).delegate.picked();
            Some(picked.into_iter().map(str::to_string).collect())
        };

        // The list comes with what the extension had ticked. Enter ticks
        // the row in hand and the list stays; the first row answers.
        wait_for(cx, "the list", &|cx| picked(cx).is_some());
        assert_eq!(cx.read(|cx| picked(cx)).unwrap(), ["beta"]);
        cx.simulate_keystrokes("enter");
        assert_eq!(cx.read(|cx| picked(cx)).unwrap(), ["alpha", "beta"]);
        cx.simulate_keystrokes("down enter");
        assert_eq!(cx.read(|cx| picked(cx)).unwrap(), ["alpha"]);
        // What is typed narrows the rows, and the row that answers stays.
        cx.simulate_input("gam");
        cx.simulate_keystrokes("enter");
        assert_eq!(cx.read(|cx| picked(cx)).unwrap(), ["alpha", "gamma"]);
        cx.simulate_keystrokes("up enter");
        // The second list is answered with nothing ticked: an empty
        // answer, which is not the same as none.
        wait_for(cx, "the second list", &|cx| {
            picked(cx).is_some_and(|picked| picked.is_empty())
        });
        cx.simulate_keystrokes("up enter");

        // A password is typed in stars.
        let line = |cx: &App| {
            let modal = ws.read(cx).modal.as_ref()?;
            let picker = modal.view.clone();
            picker
                .downcast::<Picker<crate::extension_ask::AskInput>>()
                .ok()
        };
        wait_for(cx, "the line to type", &|cx| line(cx).is_some());
        assert!(cx.read(|cx| line(cx).unwrap().read(cx).is_masked(cx)));
        cx.simulate_input("s3cret");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "its answers", &|cx| {
            let store = store.read(cx);
            let code = store.code("Acme.many").unwrap();
            let said = "picked alpha,gamma; none []; secret s3cret";
            code.said.iter().any(|(_, text)| text == said)
        });
    }

    #[gpui::test]
    fn an_extension_shows_pages_in_tabs(cx: &mut TestAppContext) {
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else {
            eprintln!("skipped: no node to run an extension with");
            return;
        };
        let (base, _) = serve(vec![
            (
                "/api/-/search",
                Served::ok(br#"{"extensions":[]}"#.to_vec()),
            ),
            ("/extensions", Served::ok(br#"{"data":[]}"#.to_vec())),
        ]);
        let (_config, store, ws, cx) = extension_setup(cx, "ext-pages", &base);
        let root = cx.read(|cx| ws.read(cx).root(cx));
        let folder = cx.read(|cx| store.read(cx).root.clone());
        let dir = folder.join("vscode/acme.pages");
        write_file(
            &dir.join("package.json"),
            r#"{ "name": "pages", "publisher": "Acme", "version": "1.0.0", "main": "main.js",
  "activationEvents": ["onCommand:pages.open"],
  "contributes": {
    "views": { "explorer": [{ "id": "pages.side", "name": "Side page", "type": "webview" }] },
    "customEditors": [{ "viewType": "pages.cat", "displayName": "Cat viewer", "selector": [{ "filenamePattern": "*.cat" }] }]
  } }"#,
        );
        write_file(
            &dir.join("main.js"),
            r#"const vscode = require('vscode');
exports.activate = (context) => {
  let page;
  context.subscriptions.push(
    vscode.commands.registerCommand('pages.open', () => {
      page = vscode.window.createWebviewPanel('pages.one', 'Page one', vscode.ViewColumn.One, { enableScripts: true });
      page.webview.html = '<h1>One</h1>';
      page.onDidDispose(() => console.log('page one closed'));
    }),
    vscode.commands.registerCommand('pages.post', () => page.webview.postMessage({ hello: 1 })),
    vscode.window.registerWebviewViewProvider('pages.side', {
      resolveWebviewView: (view) => { view.webview.html = '<b>side</b>'; },
    }),
    vscode.window.registerCustomEditorProvider('pages.cat', {
      resolveCustomTextEditor: (document, panel) => { panel.webview.html = `<pre>${document.getText()}</pre>`; },
    }),
  );
};"#,
        );
        std::fs::write(root.join("tom.cat"), "meow").unwrap();
        store.update(cx, |store, cx| {
            store.world = Some(std::sync::Arc::new(NodeOnly(
                node.to_string_lossy().into_owned(),
            )));
            store.scan(cx);
        });
        wait_for(cx, "the extension", &|cx| {
            store.read(cx).find(Origin::VsCode, "Acme.pages").is_some()
        });
        store.update(cx, |store, cx| {
            store.allow(Origin::VsCode, "Acme.pages", cx)
        });
        // The pages this window has, by their titles, and the one in front.
        let pages = |cx: &App| -> (Vec<String>, Option<String>) {
            let ws = ws.read(cx);
            let titles = ws
                .web_pages
                .iter()
                .map(|(page, _)| page.read(cx).model.title.clone());
            let front = ws
                .web_front
                .as_ref()
                .map(|(page, _)| page.read(cx).model.title.clone());
            (titles.collect(), front)
        };
        let html = |cx: &App, title: &str| {
            let ws = ws.read(cx);
            let page = ws
                .web_pages
                .iter()
                .find(|(page, _)| page.read(cx).model.title == title);
            page.map(|(page, _)| page.read(cx).model.html.clone())
        };
        assert!(cx.read(|cx| pages(cx)).0.is_empty());
        assert!(cx.read(|cx| ws.read(cx).active_editor().is_some()));

        // A page an extension opens is a tab of the window, in front of
        // the file that was there, and names the window.
        let ran = |cx: &mut VisualTestContext, command: &str| {
            let task = store.update(cx, |store, cx| {
                store.run_command(command, serde_json::Value::Null, cx)
            });
            let answer = std::rc::Rc::new(std::cell::RefCell::new(None));
            let got = answer.clone();
            cx.foreground_executor()
                .spawn(async move { *got.borrow_mut() = Some(task.await) })
                .detach();
            for _ in 0..500 {
                cx.run_until_parked();
                if answer.borrow().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let answer = answer.borrow_mut().take();
            answer.expect("the command did not answer")
        };
        ran(cx, "pages.open").unwrap();
        wait_for(cx, "the page", &|cx| {
            pages(cx) == (vec!["Page one".to_string()], Some("Page one".to_string()))
                && html(cx, "Page one").as_deref() == Some("<h1>One</h1>")
        });
        assert!(cx.read(|cx| ws.read(cx).active_editor().is_none()));
        assert_eq!(cx.read(|cx| ws.read(cx).front_title(cx)), "Page one");
        // What the extension sends it is taken for the page.
        assert_eq!(ran(cx, "pages.post"), Ok(serde_json::json!(true)));

        // A file's tab chosen, the file is in front again and the page
        // keeps its tab; closed by its own button, the extension hears.
        let tab = bounds_soon(cx, "web-tab-0");
        ws.update_in(cx, |w, window, cx| {
            let pane = w.active_pane;
            w.activate(pane, 0, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| pages(cx)).1, None);
        assert!(cx.read(|cx| ws.read(cx).active_editor().is_some()));
        cx.simulate_click(tab.center(), gpui::Modifiers::default());
        assert_eq!(cx.read(|cx| pages(cx)).1.as_deref(), Some("Page one"));
        let close = bounds_soon(cx, "web-close-0");
        cx.simulate_click(close.center(), gpui::Modifiers::default());
        wait_for(cx, "the extension to hear", &|cx| {
            let store = store.read(cx);
            let code = store.code("Acme.pages").unwrap();
            code.said.iter().any(|(_, text)| text == "page one closed")
        });
        assert!(cx.read(|cx| pages(cx)).0.is_empty());
        assert!(cx.read(|cx| ws.read(cx).active_editor().is_some()));

        // An editor of the extension's is offered for a file its pattern
        // names, in the tree's menu, and opens the file as a page.
        let offered = |cx: &App, name: &str| {
            let store = store.read(cx);
            store.menu("explorer/context", &store.facts(), Some(&root.join(name)))
        };
        assert!(cx.read(|cx| offered(cx, "App.vue")).is_empty());
        let with = cx.read(|cx| offered(cx, "tom.cat"));
        assert_eq!(with.len(), 1);
        assert_eq!(with[0].title, "Open with Cat viewer");
        cx.dispatch_action(with[0].action.clone());
        wait_for(cx, "the file as a page", &|cx| {
            html(cx, "tom.cat").as_deref() == Some("<pre>meow</pre>")
        });

        // A view that is a page is in the list of views, and chosen there
        // it opens in a tab.
        cx.dispatch_action(ShowExtensionViews);
        cx.run_until_parked();
        let panel = cx.read(|cx| ws.read(cx).extension_views.clone());
        wait_for(cx, "the view", &|cx| panel.read(cx).has("Side page"));
        let side = cx.read(|cx| panel.read(cx).index("Side page").unwrap());
        panel.update(cx, |panel, cx| panel.pick(side, cx));
        wait_for(cx, "the view as a page", &|cx| {
            html(cx, "Side page").as_deref() == Some("<b>side</b>")
        });
        assert_eq!(cx.read(|cx| pages(cx)).0, ["tom.cat", "Side page"]);

        // Its code ended, its pages go with it.
        store.update(cx, |store, cx| {
            store.set_off(Origin::VsCode, "Acme.pages", true, cx)
        });
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| pages(cx)), (Vec::new(), None));
    }

    #[gpui::test]
    fn an_extension_runs_terminals_and_tasks_in_the_dock(cx: &mut TestAppContext) {
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else {
            eprintln!("skipped: no node to run an extension with");
            return;
        };
        let (base, _) = serve(vec![
            (
                "/api/-/search",
                Served::ok(br#"{"extensions":[]}"#.to_vec()),
            ),
            ("/extensions", Served::ok(br#"{"data":[]}"#.to_vec())),
        ]);
        let (_config, store, ws, cx) = extension_setup(cx, "ext-shell", &base);
        let folder = cx.read(|cx| store.read(cx).root.clone());
        let dir = folder.join("vscode/acme.shell");
        write_file(
            &dir.join("package.json"),
            r#"{ "name": "shell", "publisher": "Acme", "version": "1.0.0", "main": "main.js",
  "activationEvents": ["*"] }"#,
        );
        write_file(
            &dir.join("main.js"),
            r#"const vscode = require('vscode');
exports.activate = (context) => {
  const terminal = vscode.window.createTerminal({ name: 'Mine', shellPath: '/bin/sh' });
  vscode.window.onDidCloseTerminal((closed) => console.log(`closed ${closed.name} ${closed.exitStatus.code}`));
  vscode.tasks.registerTaskProvider('demo', {
    provideTasks: () => [
      new vscode.Task({ type: 'demo' }, vscode.TaskScope.Workspace, 'fail', 'demo',
        new vscode.ShellExecution('echo task-$((1+1)); exit 3', { executable: '/bin/sh' })),
    ],
  });
  vscode.tasks.onDidEndTaskProcess((e) => console.log(`task ${e.execution.task.name} ended with ${e.exitCode}`));
  context.subscriptions.push(
    vscode.commands.registerCommand('shell.type', () => {
      terminal.sendText('echo solder-$((40+2))');
      terminal.show();
    }),
    vscode.commands.registerCommand('shell.close', () => terminal.dispose()),
    // A terminal that is an object here, with no program behind it.
    vscode.commands.registerCommand('shell.own', () => {
      const write = new vscode.EventEmitter();
      const own = vscode.window.createTerminal({ name: 'Own', pty: {
        onDidWrite: write.event,
        open: (size) => write.fire(`own-ready ${size.columns > 0}\r\n`),
        close: () => console.log('own let go'),
        handleInput: (data) => write.fire(`[${data.toUpperCase()}]`),
      } });
      own.show();
    }),
  );
};"#,
        );
        store.update(cx, |store, cx| {
            store.world = Some(std::sync::Arc::new(NodeOnly(
                node.to_string_lossy().into_owned(),
            )));
            store.scan(cx);
        });
        wait_for(cx, "the extension", &|cx| {
            store.read(cx).find(Origin::VsCode, "Acme.shell").is_some()
        });
        store.update(cx, |store, cx| {
            store.allow(Origin::VsCode, "Acme.shell", cx)
        });
        wait_for(cx, "its code", &|cx| {
            store
                .read(cx)
                .code("Acme.shell")
                .map(|code| code.state.clone())
                == Some(CodeState::Running)
        });
        let said = |cx: &App, what: &str| {
            let store = store.read(cx);
            let code = store.code("Acme.shell").unwrap();
            code.said.iter().any(|(_, text)| text == what)
        };
        let shows = |cx: &App, terminal: usize, line: &str| {
            let terminals = &ws.read(cx).terminals;
            let Some((terminal, _)) = terminals.get(terminal) else {
                return false;
            };
            terminal.read(cx).visible_text().iter().any(|l| l == line)
        };
        // A terminal it only keeps ready takes no room in the dock.
        assert!(cx.read(|cx| ws.read(cx).terminals.is_empty()));

        // Typed into, it is a terminal of the dock, under its name, and
        // what was typed ran in it.
        store.update(cx, |store, cx| {
            store
                .run_command("shell.type", serde_json::Value::Null, cx)
                .detach()
        });
        wait_for(cx, "what it typed to run", &|cx| shows(cx, 0, "solder-42"));
        assert_eq!(
            cx.read(|cx| ws.read(cx).terminals[0].0.read(cx).title()),
            "Mine"
        );
        assert!(cx.read(|cx| ws.read(cx).bottom == Some(Panel::Terminal)));

        // Its tasks are in the list of tasks, and one chosen runs in a
        // terminal of its own. The extension hears what it ended with,
        // and the tab stays, for what it wrote to be read.
        cx.dispatch_action(RunExtensionTask);
        wait_for(cx, "the tasks", &|cx| {
            let Some(modal) = &ws.read(cx).modal else {
                return false;
            };
            let picker = modal
                .view
                .clone()
                .downcast::<Picker<crate::extension_ask::TaskPick>>();
            picker.is_ok_and(|picker| !picker.read(cx).delegate.is_loading())
        });
        cx.simulate_input("fail");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the task to end", &|cx| {
            said(cx, "task fail ended with 3")
        });
        assert!(cx.read(|cx| shows(cx, 1, "task-2")));
        let ended = cx.read(|cx| {
            let task = ws.read(cx).terminals[1].0.read(cx);
            (task.title().to_string(), task.exited, task.exit_code)
        });
        assert_eq!(ended, ("fail".to_string(), true, Some(3)));

        // Closed by the extension, its terminal leaves the dock, and it
        // hears that too.
        store.update(cx, |store, cx| {
            store
                .run_command("shell.close", serde_json::Value::Null, cx)
                .detach()
        });
        wait_for(cx, "its terminal to close", &|cx| {
            ws.read(cx).terminals.len() == 1 && said(cx, "closed Mine undefined")
        });

        // A terminal the extension draws itself is a tab of the dock like
        // the others: it is told the terminal's size, what it writes is
        // shown, and what is typed reaches it key by key and is not shown
        // by anyone but itself.
        store.update(cx, |store, cx| {
            store
                .run_command("shell.own", serde_json::Value::Null, cx)
                .detach()
        });
        wait_for(cx, "the terminal it draws", &|cx| {
            shows(cx, 1, "own-ready true")
        });
        assert_eq!(
            cx.read(|cx| ws.read(cx).terminals[1].0.read(cx).title()),
            "Own"
        );
        let own = cx.read(|cx| ws.read(cx).terminals[1].0.clone());
        own.update(cx, |terminal, _| terminal.write(b"ok".to_vec()));
        wait_for(cx, "what was typed to reach it", &|cx| {
            let terminal = ws.read(cx).terminals[1].0.read(cx);
            let shown = terminal.visible_text().join("");
            shown.contains("[O][K]") || shown.contains("[OK]")
        });
        // Its tab closed, the extension's object is let go.
        ws.update_in(cx, |w, window, cx| w.remove_terminal(&own, window, cx));
        // The terminal ends when nothing holds it, this test included.
        drop(own);
        wait_for(cx, "the object to be let go", &|cx| said(cx, "own let go"));
    }

    #[gpui::test]
    fn a_debugger_an_extension_sets_up_in_code_debugs_a_file(cx: &mut TestAppContext) {
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else {
            eprintln!("skipped: no node to run an extension with");
            return;
        };
        let root = db::testing::dir("ws-coded-debug").canonicalize().unwrap();
        let app = root.join("app.demo");
        std::fs::write(&app, "one\ntwo\nthree\n").unwrap();
        let data = db::testing::dir("ws-coded-debug-data");
        let installed = data.join("extensions/vscode/acme.coded");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_dap_stdio.py"),
            installed.join("adapter.py"),
        )
        .unwrap();
        // Two debuggers whose manifest names no adapter: the code says
        // what each is. One is a program the code names; the other is an
        // object in the extension's own code.
        write_file(
            &installed.join("package.json"),
            r#"{ "name": "coded", "publisher": "Acme", "version": "1.0.0", "main": "main.js",
  "activationEvents": ["onDebugResolve:coded"],
  "contributes": {
    "languages": [{ "id": "demo", "extensions": [".demo"] }],
    "breakpoints": [{ "language": "demo" }],
    "debuggers": [{ "type": "coded", "label": "Coded" }, { "type": "inline", "label": "Inline" }]
  } }"#,
        );
        write_file(
            &installed.join("main.js"),
            r#"const vscode = require('vscode');
// An adapter that is an object here: it stops on the first breakpoint of
// the program it was launched for, and ends when told to go on.
class Inline {
  constructor() {
    this.sent = new vscode.EventEmitter();
    this.onDidSendMessage = this.sent.event;
    this.lines = {};
    this.seq = 0;
  }
  say(message) {
    this.sent.fire({ seq: ++this.seq, ...message });
  }
  handleMessage(request) {
    const args = request.arguments || {};
    let body = {};
    if (request.command === 'initialize') body = { supportsConfigurationDoneRequest: true };
    if (request.command === 'launch') this.program = args.program;
    if (request.command === 'setBreakpoints') {
      this.lines[args.source.path] = (args.breakpoints || []).map((one) => one.line);
      body = { breakpoints: this.lines[args.source.path].map((line) => ({ verified: true, line })) };
    }
    if (request.command === 'threads') body = { threads: [{ id: 1, name: 'main' }] };
    if (request.command === 'stackTrace') {
      const line = (this.lines[this.program] || [1])[0];
      body = { stackFrames: [{ id: 1, name: 'inline', line, column: 1, source: { path: this.program, name: 'program' } }], totalFrames: 1 };
    }
    if (request.command === 'scopes') body = { scopes: [] };
    this.say({ type: 'response', request_seq: request.seq, success: true, command: request.command, body });
    if (request.command === 'initialize') this.say({ type: 'event', event: 'initialized', body: {} });
    if (request.command === 'configurationDone') this.say({ type: 'event', event: 'stopped', body: { reason: 'breakpoint', threadId: 1, allThreadsStopped: true } });
    if (request.command === 'continue') this.say({ type: 'event', event: 'terminated', body: {} });
  }
  dispose() {
    console.log('the adapter was let go');
  }
}
exports.activate = (context) => {
  context.subscriptions.push(
    vscode.debug.registerDebugConfigurationProvider('coded', {
      resolveDebugConfiguration(folder, launch) {
        if (launch.name === 'Not this one') return undefined;
        return { ...launch, stopOnEntry: true, folder: folder.name };
      },
    }),
    vscode.debug.registerDebugAdapterDescriptorFactory('coded', {
      createDebugAdapterDescriptor: (session) =>
        new vscode.DebugAdapterExecutable('python3', [context.asAbsolutePath('adapter.py'), context.asAbsolutePath(session.configuration.name + '.json')]),
    }),
    vscode.debug.registerDebugAdapterDescriptorFactory('inline', {
      createDebugAdapterDescriptor: () => new vscode.DebugAdapterInlineImplementation(new Inline()),
    }),
    vscode.debug.onDidStartDebugSession((session) => console.log(`started ${session.type} ${session.name}`)),
    vscode.debug.onDidTerminateDebugSession((session) => console.log(`ended ${session.type}`)),
    vscode.commands.registerCommand('coded.debug', (program, name) =>
      vscode.debug.startDebugging(undefined, { type: 'coded', request: 'launch', name, program })),
  );
};"#,
        );
        cx.executor().allow_parking();
        let (extensions, debug) = cx.update(|cx| {
            let extensions = cx.new(|cx| {
                let mut store =
                    ExtensionStore::new(data.join("extensions"), data.join("config"), cx);
                store.world = Some(std::sync::Arc::new(NodeOnly(
                    node.to_string_lossy().into_owned(),
                )));
                store
            });
            ExtensionStore::set_global(extensions.clone(), cx);
            extensions.update(cx, |s, cx| s.scan(cx));
            let debug = cx.new(|_| {
                crate::debug::DebugStore::new(
                    data.join("debug"),
                    crate::debug::AdapterSpec::JsDebug,
                )
            });
            crate::debug::DebugStore::set_global(debug.clone(), cx);
            (extensions, debug)
        });
        let (ws, cx) = setup(cx, root.clone());
        wait_for(cx, "the extensions folder", &|cx| {
            extensions.read(cx).loaded
        });
        ws.update_in(cx, |w, window, cx| {
            w.open_path(app.clone(), None, window, cx)
        });
        let offered = |cx: &App| -> Vec<String> {
            let configs = crate::debug_launch::from_extensions(&root, &app, cx);
            configs.into_iter().map(|config| config.name).collect()
        };
        let said = |cx: &App, what: &str| {
            let store = extensions.read(cx);
            let code = store.code("Acme.coded");
            code.is_some_and(|code| code.said.iter().any(|(_, text)| text == what))
        };
        // Its code was not allowed, so it has no debugger to offer: only
        // the code knows what the adapters are.
        assert!(cx.read(|cx| offered(cx)).is_empty());
        extensions.update(cx, |store, cx| {
            store.allow(Origin::VsCode, "Acme.coded", cx)
        });
        assert_eq!(
            cx.read(|cx| offered(cx)),
            ["coded app.demo", "inline app.demo"]
        );
        // Offering them started nothing: it waits to be asked.
        assert!(cx.read(|cx| extensions.read(cx).code("Acme.coded").is_none()));

        // The first: its code is started, goes over the launch, and names
        // a program for the adapter. The run stops on the breakpoint.
        let configs = cx.read(|cx| crate::debug_launch::from_extensions(&root, &app, cx));
        debug.update(cx, |s, cx| {
            s.toggle(&app, 2, cx);
            s.start(configs[0].clone(), root.clone(), cx)
        });
        wait_for(cx, "the pause in app.demo", &|cx| {
            paused_line(&debug, cx) == Some(2)
        });
        let launch: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(installed.join("coded app.demo.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(launch["program"], app.display().to_string());
        // What its provider added to the launch.
        assert_eq!(launch["stopOnEntry"], true);
        assert_eq!(
            launch["folder"],
            root.file_name().unwrap().to_string_lossy().as_ref()
        );
        wait_for(cx, "the extension to hear of it", &|cx| {
            said(cx, "started coded coded app.demo")
        });
        debug.update(cx, |s, cx| s.stop(cx));
        cx.run_until_parked();
        assert!(cx.read(|cx| !debug.read(cx).state.active()));

        // The second: its adapter is an object in the extension's code,
        // reached at a port of this machine like any adapter.
        debug.update(cx, |s, cx| s.start(configs[1].clone(), root.clone(), cx));
        wait_for(cx, "the pause by the adapter in code", &|cx| {
            paused_line(&debug, cx) == Some(2)
                && debug
                    .read(cx)
                    .paused
                    .as_ref()
                    .unwrap()
                    .frame()
                    .unwrap()
                    .name
                    == "inline"
        });
        debug.update(cx, |s, cx| s.resume(cx));
        wait_for(cx, "the run to end", &|cx| !debug.read(cx).state.active());
        wait_for(cx, "the extension to hear of the end", &|cx| {
            said(cx, "ended inline") && said(cx, "the adapter was let go")
        });

        // The extension starts a run itself, with a launch of its own.
        let running = extensions.update(cx, |store, cx| {
            let args = serde_json::json!([app.display().to_string(), "From code"]);
            store.run_command("coded.debug", args, cx)
        });
        wait_for(cx, "the pause in the run it started", &|cx| {
            paused_line(&debug, cx) == Some(2)
        });
        assert_eq!(
            cx.executor().block_test(running),
            Ok(serde_json::json!(true))
        );
        assert!(installed.join("From code.json").is_file());
        debug.update(cx, |s, cx| s.stop(cx));
        cx.run_until_parked();
        // A launch its provider calls off is not started, and says so.
        let called_off = extensions.update(cx, |store, cx| {
            let launch = extension::host::DebugLaunch {
                adapter: "coded".into(),
                ..Default::default()
            };
            let given = serde_json::json!({ "type": "coded", "name": "Not this one" });
            store.debug_adapter("Acme.coded", launch, Some(given), &root, cx)
        });
        let answer = std::rc::Rc::new(std::cell::RefCell::new(None));
        let got = answer.clone();
        cx.foreground_executor()
            .spawn(async move { *got.borrow_mut() = Some(called_off.await) })
            .detach();
        for _ in 0..500 {
            cx.run_until_parked();
            if answer.borrow().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let answer = answer.borrow_mut().take();
        assert_eq!(
            answer,
            Some(Err("The extension called the launch off".to_string()))
        );
    }

    #[gpui::test]
    fn what_an_extension_contributes_is_in_the_palette_the_menus_and_the_keys(
        cx: &mut TestAppContext,
    ) {
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else {
            eprintln!("skipped: no node to run an extension with");
            return;
        };
        let _languages = extension_languages();
        let (base, _) = serve(vec![
            (
                "/api/-/search",
                Served::ok(br#"{"extensions":[]}"#.to_vec()),
            ),
            ("/extensions", Served::ok(br#"{"data":[]}"#.to_vec())),
        ]);
        let (_config, store, ws, cx) = extension_setup(cx, "ext-contributes", &base);
        let root = cx.read(|cx| ws.read(cx).root(cx));
        let folder = cx.read(|cx| store.read(cx).root.clone());
        extension::testing::vscode_extension(&folder.join("vscode/acme.demo"));
        store.update(cx, |store, cx| {
            store.world = Some(std::sync::Arc::new(NodeOnly(
                node.to_string_lossy().into_owned(),
            )));
            store.scan(cx);
        });
        wait_for(cx, "the extension", &|cx| {
            store.read(cx).find(Origin::VsCode, "Acme.demo").is_some()
        });
        let titles = |cx: &App| -> Vec<String> {
            let facts = ws.read(cx).extension_facts(cx);
            let offered = store.read(cx).palette(&facts);
            offered.into_iter().map(|offered| offered.title).collect()
        };
        // The note its command leaves in the status bar when it ran.
        let ran = |cx: &App, with: &str| {
            let said = format!("ran {with}");
            store.read(cx).bar().iter().any(|item| item.text == said)
        };
        let key = if cfg!(target_os = "macos") {
            "cmd-alt-r"
        } else {
            "ctrl-alt-r"
        };

        // Its code was not allowed: it has nothing in the palette, and its
        // keys are nobody's.
        assert!(cx.read(|cx| titles(cx)).is_empty());
        store.update(cx, |store, cx| store.allow(Origin::VsCode, "Acme.demo", cx));
        // Allowed, the palette has the command its manifest names, under
        // the name it gives it. One it keeps out of the palette is not
        // there, nor one that cannot be run yet.
        assert_eq!(cx.read(|cx| titles(cx)), ["Demo: Run"]);
        cx.dispatch_action(ToggleCommandPalette);
        cx.run_until_parked();
        cx.simulate_input("demo: run");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the command to run", &|cx| ran(cx, "[]"));
        // It said that its other command may be run now, and so it is
        // there.
        wait_for(cx, "the other command", &|cx| {
            titles(cx) == ["Demo: Run", "Spin"]
        });

        // The key it binds is bound where its condition says: in a file
        // of its language, not in another.
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let focus = cx.read(|cx| editor.focus_handle(cx));
        cx.update(|window, _| window.focus(&focus));
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        let notes = root.join("notes.dm");
        std::fs::write(&notes, "if 1\n").unwrap();
        ws.update_in(cx, |w, window, cx| {
            w.open_path(notes.clone(), None, window, cx)
        });
        wait_for(cx, "notes.dm", &|cx| {
            ws.read(cx)
                .active_editor()
                .is_some_and(|editor| editor.read(cx).path(cx) == Some(notes.as_path()))
                && ws.read(cx).extension_facts(cx)["editorLangId"] == "demo"
        });
        assert!(
            cx.read(|cx| ran(cx, "[]")),
            "the key ran it in a file of another language"
        );
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let focus = cx.read(|cx| editor.focus_handle(cx));
        cx.update(|window, _| window.focus(&focus));
        cx.simulate_keystrokes(key);
        wait_for(cx, "the key to run it", &|cx| ran(cx, r#"["from a key"]"#));

        // The right button in the file opens what it put in the editor's
        // menu, and the command is given the file.
        let body = cx.debug_bounds("pane-body-0").unwrap().center();
        let none = gpui::Modifiers::default();
        cx.simulate_mouse_down(body, MouseButton::Right, none);
        cx.simulate_mouse_up(body, MouseButton::Right, none);
        cx.run_until_parked();
        let item = bounds_soon(cx, "editor-menu-0");
        cx.simulate_click(item.center(), none);
        wait_for(cx, "the menu's command", &|cx| ran(cx, r#"["notes.dm"]"#));
        assert!(cx.read(|cx| ws.read(cx).editor_menu.is_none()));
        // The tree's menu has it for a file with its ending, and for no
        // other; in a file of another language the editor has no menu.
        let in_tree = |cx: &App, name: &str| {
            let store = store.read(cx);
            let mut facts = store.facts();
            let ending = name.rsplit_once('.').map_or("", |(_, ending)| ending);
            facts.insert("resourceExtname".into(), format!(".{ending}").into());
            let offered = store.menu("explorer/context", &facts, Some(&root.join(name)));
            offered.into_iter().map(|o| o.title).collect::<Vec<_>>()
        };
        assert_eq!(cx.read(|cx| in_tree(cx, "notes.dm")), ["Demo: Run"]);
        assert!(cx.read(|cx| in_tree(cx, "App.vue")).is_empty());
        ws.update_in(cx, |w, window, cx| {
            w.open_path(root.join("App.vue"), None, window, cx)
        });
        wait_for(cx, "App.vue", &|cx| {
            ws.read(cx).extension_facts(cx)["editorLangId"] != "demo"
        });
        cx.simulate_mouse_down(body, MouseButton::Right, none);
        cx.simulate_mouse_up(body, MouseButton::Right, none);
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).editor_menu.is_none()));

        // A key of two strokes, bound everywhere: its command ends the
        // extension's process, which is how the test sees that it ran.
        cx.simulate_keystrokes("ctrl-k ctrl-q");
        wait_for(cx, "the second key", &|cx| {
            matches!(
                store.read(cx).code("Acme.demo").map(|code| &code.state),
                Some(CodeState::Stopped(_))
            )
        });

        // Turned off, nothing of it is in the palette.
        store.update(cx, |store, cx| {
            store.set_off(Origin::VsCode, "Acme.demo", true, cx)
        });
        assert!(cx.read(|cx| titles(cx)).is_empty());
    }

    /// What `language_server_features` asks of a server, asked of the code
    /// of a VS Code extension: the same file, the same keys.
    #[gpui::test]
    fn an_extension_gives_language_features_in_code(cx: &mut TestAppContext) {
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else {
            eprintln!("skipped: no node to run an extension with");
            return;
        };
        let (base, _) = serve(vec![
            (
                "/api/-/search",
                Served::ok(br#"{"extensions":[]}"#.to_vec()),
            ),
            ("/extensions", Served::ok(br#"{"data":[]}"#.to_vec())),
        ]);
        let (_config, store, ws, cx) = extension_setup(cx, "ext-lang", &base);
        let root = cx.read(|cx| ws.read(cx).root(cx));
        let folder = cx.read(|cx| store.read(cx).root.clone());
        let dir = folder.join("vscode/acme.lang");
        write_file(
            &dir.join("package.json"),
            r#"{ "name": "lang", "publisher": "Acme", "version": "1.0.0", "main": "main.js",
  "activationEvents": ["onLanguage:plaintext"] }"#,
        );
        write_file(
            &dir.join("main.js"),
            r#"const vscode = require('vscode');
exports.activate = (context) => {
  // A file of no language Solder has a server for, or a grammar.
  const selector = { scheme: 'file', language: 'plaintext' };
  const legend = new vscode.SemanticTokensLegend(['keyword']);
  const problems = vscode.languages.createDiagnosticCollection('lang');
  const check = (document) => {
    if (document.languageId !== 'plaintext') return;
    const found = [];
    for (let line = 0; line < document.lineCount; line++) {
      const text = document.lineAt(line).text;
      for (const [word, severity] of [['TODO', vscode.DiagnosticSeverity.Warning], ['boom', vscode.DiagnosticSeverity.Error]]) {
        const at = text.indexOf(word);
        if (at < 0) continue;
        const one = new vscode.Diagnostic(new vscode.Range(line, at, line, at + word.length), `${word} here`, severity);
        one.source = 'lang';
        found.push(one);
      }
    }
    problems.set(document.uri, found);
  };
  vscode.workspace.textDocuments.forEach(check);
  const all = (document, word) => {
    const ranges = [];
    const text = document.getText();
    for (let at = text.indexOf(word); at >= 0; at = text.indexOf(word, at + 1)) {
      ranges.push(new vscode.Range(document.positionAt(at), document.positionAt(at + word.length)));
    }
    return ranges;
  };
  context.subscriptions.push(
    problems,
    vscode.workspace.onDidOpenTextDocument(check),
    vscode.workspace.onDidChangeTextDocument((e) => check(e.document)),
    vscode.languages.registerCompletionItemProvider(selector, {
      provideCompletionItems() {
        const item = new vscode.CompletionItem('println', vscode.CompletionItemKind.Function);
        item.insertText = new vscode.SnippetString('println!("$1")');
        item.detail = 'macro';
        return new vscode.CompletionList([item, new vscode.CompletionItem('print', vscode.CompletionItemKind.Function)]);
      },
    }),
    vscode.languages.registerHoverProvider(selector, {
      provideHover(document, position) {
        const range = document.getWordRangeAtPosition(position);
        return range ? new vscode.Hover(new vscode.MarkdownString(`**${document.getText(range)}** is a word`), range) : undefined;
      },
    }),
    vscode.languages.registerDefinitionProvider(selector, {
      provideDefinition: (document) => new vscode.Location(document.uri, new vscode.Range(0, 3, 0, 9)),
    }),
    vscode.languages.registerRenameProvider(selector, {
      provideRenameEdits(document, position, newName) {
        const edit = new vscode.WorkspaceEdit();
        const word = document.getText(document.getWordRangeAtPosition(position));
        for (const range of all(document, word)) edit.replace(document.uri, range, newName);
        return edit;
      },
    }),
    vscode.languages.registerDocumentFormattingEditProvider(selector, {
      provideDocumentFormattingEdits(document) {
        const edits = [];
        for (let line = 0; line < document.lineCount; line++) {
          const text = document.lineAt(line).text;
          const kept = text.trimEnd().length;
          if (kept < text.length) edits.push(vscode.TextEdit.delete(new vscode.Range(line, kept, line, text.length)));
        }
        return edits;
      },
    }),
    vscode.languages.registerCodeActionsProvider(selector, {
      provideCodeActions(document) {
        const header = new vscode.CodeAction('Add header', vscode.CodeActionKind.QuickFix);
        header.target = document.uri;
        const touch = new vscode.CodeAction('Touch', vscode.CodeActionKind.Refactor);
        touch.command = { title: 'Touch', command: 'lang.touch', arguments: [document.uri] };
        return [header, touch];
      },
      // What the first one changes is worked out only once it is chosen.
      resolveCodeAction(action) {
        action.edit = new vscode.WorkspaceEdit();
        action.edit.insert(action.target, new vscode.Position(0, 0), '// header\n');
        return action;
      },
    }),
    vscode.languages.registerCodeLensProvider(selector, {
      provideCodeLenses: (document) => [new vscode.CodeLens(new vscode.Range(0, 0, 0, 2), { title: 'Count lines', command: 'lang.count', arguments: [document.uri] })],
    }),
    vscode.commands.registerCommand('lang.touch', async (uri) => {
      const edit = new vscode.WorkspaceEdit();
      edit.insert(uri, new vscode.Position(0, 0), '// touched\n');
      await vscode.workspace.applyEdit(edit);
    }),
    vscode.commands.registerCommand('lang.count', (uri) => {
      const document = vscode.workspace.textDocuments.find((one) => one.uri.toString() === uri.toString());
      vscode.window.showInformationMessage(`${document.lineCount} lines`);
    }),
    vscode.languages.registerInlayHintsProvider(selector, {
      provideInlayHints: () => [new vscode.InlayHint(new vscode.Position(0, 2), ' (a function)')],
    }),
    vscode.languages.registerDocumentSemanticTokensProvider(selector, {
      provideDocumentSemanticTokens(document) {
        const builder = new vscode.SemanticTokensBuilder(legend);
        const word = /[A-Za-z_]+/.exec(document.lineAt(0).text);
        if (word) builder.push(new vscode.Range(0, word.index, 0, word.index + word[0].length), 'keyword');
        return builder.build();
      },
    }, legend),
    vscode.languages.registerDocumentSymbolProvider(selector, {
      provideDocumentSymbols: () => [new vscode.DocumentSymbol('helper', 'fn', vscode.SymbolKind.Function, new vscode.Range(0, 0, 0, 14), new vscode.Range(0, 3, 0, 9))],
    }),
    vscode.languages.registerSignatureHelpProvider(selector, {
      provideSignatureHelp(document, position) {
        const help = new vscode.SignatureHelp();
        const info = new vscode.SignatureInformation('assist(a: i32, b: i32)');
        info.parameters = [new vscode.ParameterInformation('a: i32'), new vscode.ParameterInformation('b: i32')];
        help.signatures = [info];
        const before = document.lineAt(position.line).text.slice(0, position.character);
        help.activeParameter = (before.slice(before.lastIndexOf('(')).match(/,/g) || []).length;
        return help;
      },
    }, '(', ','),
  );
};"#,
        );
        let file = root.join("notes.txt");
        std::fs::write(&file, "fn helper() {}\n// TODO fix\n").unwrap();
        store.update(cx, |store, cx| {
            store.world = Some(std::sync::Arc::new(NodeOnly(
                node.to_string_lossy().into_owned(),
            )));
            store.scan(cx);
        });
        wait_for(cx, "the extension", &|cx| {
            store.read(cx).find(Origin::VsCode, "Acme.lang").is_some()
        });
        store.update(cx, |store, cx| store.allow(Origin::VsCode, "Acme.lang", cx));
        ws.update_in(cx, |w, window, cx| {
            w.open_path(file.clone(), None, window, cx)
        });
        wait_for(cx, "notes.txt", &|cx| {
            ws.read(cx)
                .active_editor()
                .is_some_and(|editor| editor.read(cx).path(cx) == Some(file.as_path()))
        });
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        let focus = cx.read(|cx| editor.focus_handle(cx));
        cx.update(|window, _| window.focus(&focus));
        let wait = wait_for;

        // What it reports is in the file as a server's diagnostics are.
        wait(cx, "diagnostics", &|cx| {
            !editor.read(cx).doc(cx).diagnostics().is_empty()
        });
        assert_eq!(
            cx.read(|cx| store.read(cx).code("Acme.lang").unwrap().languages.clone()),
            ["*", "plaintext"]
        );
        let diag = cx.read(|cx| editor.read(cx).doc(cx).diagnostics()[0].clone());
        assert_eq!(diag.range, 18..22);
        assert_eq!(diag.severity, crate::document::Severity::Warning);
        assert_eq!(diag.message, "TODO here");
        // It draws into the text as a server does: a hint in the line, and
        // a color for a word of a file that has no grammar at all.
        wait(cx, "its hint and its color", &|cx| {
            let editor = editor.read(cx);
            let doc = editor.doc(cx);
            !doc.inlays().is_empty() && !doc.semantic().is_empty()
        });
        let drawn = cx.read(|cx| {
            let editor = editor.read(cx);
            let doc = editor.doc(cx);
            (doc.inlays()[0].clone(), doc.semantic()[0].clone())
        });
        assert_eq!(
            drawn,
            (
                crate::document::Inlay {
                    offset: 2,
                    text: " (a function)".into()
                },
                (0..2, syntax::HighlightKind::Keyword)
            )
        );
        // It reads what is typed as it is typed.
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

        // Completion: the menu opens as a word is typed, and Enter puts
        // in the snippet with the cursor in its place.
        cx.simulate_keystrokes("enter");
        cx.simulate_input("pri");
        wait(cx, "completions", &|cx| {
            editor.read(cx).completion.is_some()
        });
        cx.simulate_input("ntl");
        let top = cx.read(|cx| {
            let editor = editor.read(cx);
            let menu = editor.completion.as_ref().unwrap();
            menu.selected_item().unwrap().label.clone()
        });
        assert_eq!(top, "println");
        cx.simulate_keystrokes("enter");
        let text = cx.read(|cx| editor.read(cx).text(cx));
        assert!(text.ends_with("boom\nprintln!(\"\")"), "{text:?}");
        assert_eq!(
            cx.read(|cx| editor.read(cx).newest_range()).start,
            text.len() - 2
        );

        // Hover, definition, rename and formatting.
        cx.dispatch_action(crate::editor::MoveToStart);
        cx.simulate_keystrokes("right right right right");
        cx.dispatch_action(crate::editor::ShowHover);
        wait(cx, "hover", &|cx| editor.read(cx).hover.is_some());
        cx.simulate_keystrokes("escape");
        cx.dispatch_action(crate::editor::MoveToStart);
        cx.simulate_keystrokes("f12");
        wait(cx, "definition", &|cx| {
            editor.read(cx).newest_range() == (3..9)
        });
        cx.simulate_keystrokes("f2");
        cx.simulate_input("assist");
        cx.simulate_keystrokes("enter");
        wait(cx, "rename", &|cx| {
            editor.read(cx).text(cx).starts_with("fn assist()")
        });
        cx.simulate_keystrokes("end");
        cx.simulate_input("   ");
        cx.simulate_keystrokes("shift-alt-f");
        wait(cx, "formatting", &|cx| {
            editor.read(cx).text(cx).starts_with("fn assist() {}\n")
        });

        // Code actions: one whose edit is worked out when it is chosen,
        // one that runs a command of the extension, which edits the file.
        cx.simulate_keystrokes("secondary-.");
        wait(cx, "code actions", &|cx| ws.read(cx).modal.is_some());
        cx.simulate_keystrokes("enter");
        wait(cx, "the edit worked out late", &|cx| {
            editor.read(cx).text(cx).starts_with("// header\n")
        });
        cx.simulate_keystrokes("secondary-.");
        wait(cx, "code actions", &|cx| ws.read(cx).modal.is_some());
        cx.simulate_input("touch");
        cx.simulate_keystrokes("enter");
        wait(cx, "the command's edit", &|cx| {
            editor.read(cx).text(cx).starts_with("// touched\n")
        });
        // A code lens of the line is among what can be done with it.
        cx.dispatch_action(crate::editor::MoveToStart);
        cx.simulate_keystrokes("secondary-.");
        wait(cx, "code actions", &|cx| ws.read(cx).modal.is_some());
        cx.simulate_input("count");
        cx.simulate_keystrokes("enter");
        wait(cx, "the lens to run", &|cx| {
            store
                .read(cx)
                .bar()
                .iter()
                .any(|item| item.text == "6 lines")
        });

        // The file's symbols are the extension's.
        cx.simulate_keystrokes("secondary-shift-o");
        wait(cx, "symbols", &|cx| ws.read(cx).modal.is_some());
        cx.simulate_input("help");
        cx.simulate_keystrokes("enter");
        wait(cx, "the symbol", &|cx| {
            ws.read(cx).modal.is_none() && editor.read(cx).newest_range().start == 3
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

        // Its code ended, what it reported goes with it.
        store.update(cx, |store, cx| {
            store.set_off(Origin::VsCode, "Acme.lang", true, cx)
        });
        wait(cx, "its diagnostics to go", &|cx| {
            editor.read(cx).doc(cx).diagnostics().is_empty()
        });
    }

    /// What extensions reach outside their sandbox through, for the test
    /// below: the server is "on the PATH" as a script that starts the mock
    /// language server, and nothing else is available.
    struct ServerOnPath(&'static str, String);

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
            (binary == self.0).then(|| self.1.clone())
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
        // The user has options and settings of their own for this server.
        // Its extension does not ask for them, so the editor adds them to
        // what the extension says.
        cx.update(|_, cx| {
            let mut settings = Settings::default();
            settings.language_servers.insert(
                "vscode-html-language-server".into(),
                settings::ServerOverride {
                    initialization_options: Some(
                        serde_json::json!({ "embeddedLanguages": { "css": true } }),
                    ),
                    settings: Some(
                        serde_json::json!({ "html": { "format": { "enable": false } } }),
                    ),
                    ..Default::default()
                },
            );
            cx.set_global(settings);
        });
        let world = ServerOnPath(
            "vscode-html-language-server",
            script.to_string_lossy().into_owned(),
        );
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
            Some(serde_json::json!({
                "provideFormatter": true,
                "embeddedLanguages": { "css": true }
            }))
        );
        assert_eq!(
            sent("configuration"),
            Some(serde_json::json!({ "html": { "format": { "enable": false } } }))
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
                let world = ServerOnPath(
                    "vscode-html-language-server",
                    script.to_string_lossy().into_owned(),
                );
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

    /// The world of the Vue test: Node is a script that starts the mock
    /// server in Vue's role, and npm "installs" by making the files the
    /// extension then looks for.
    struct VueWorld {
        node: String,
        /// The user's settings, as the store gives them to extensions.
        settings: extension::world::SettingsFor,
    }

    impl extension::host::World for VueWorld {
        fn node(&self) -> Result<String, String> {
            Ok(self.node.clone())
        }
        fn npm_latest(&self, _: &str) -> Result<String, String> {
            Ok("3.0.0".into())
        }
        fn npm_install(&self, dir: &Path, package: &str, version: &str) -> Result<(), String> {
            let package = dir.join("node_modules").join(package);
            write_file(
                &package.join("package.json"),
                &format!(r#"{{"version": "{version}"}}"#),
            );
            write_file(&package.join("bin/vue-language-server.js"), "");
            Ok(())
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
        fn which(&self, _: &str) -> Option<String> {
            None
        }
        fn env(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn status(&self, _: &str, _: extension::host::Status) {}
        fn settings(&self, category: &str, key: Option<&str>) -> Option<String> {
            (self.settings)(category, key)
        }
    }

    /// What a mock server wrote down under `what`, in order.
    fn noted(log: &Path, what: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|entry| entry[0] == what)
            .map(|entry| entry[1].clone())
            .collect()
    }

    fn executable(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        write_file(path, text);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[gpui::test]
    fn vue_sets_up_the_typescript_server_and_talks_to_it(cx: &mut TestAppContext) {
        let _languages = extension_languages();
        // A project with a TypeScript file and a Vue component, and Zed's
        // real Vue extension installed. Both servers are the mock: the
        // TypeScript one through the user's settings, Vue's as the "Node"
        // the extension is given.
        let root = db::testing::dir("ws-vue").canonicalize().unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let script = root.join("src/util.ts");
        let component = root.join("src/App.vue");
        std::fs::write(&script, "export const one = 1\n").unwrap();
        std::fs::write(
            &component,
            "<template>\n  <p>{{ msg }}</p>\n</template>\n<script setup lang=\"ts\">\nconst msg = 'hi'\n</script>\n",
        )
        .unwrap();
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_lsp.py");
        let scratch = db::testing::dir("ws-vue-servers");
        let (ts_log, vue_log) = (scratch.join("ts.jsonl"), scratch.join("vue.jsonl"));
        let typescript = scratch.join("typescript-language-server");
        executable(
            &typescript,
            &format!(
                "#!/bin/sh\nMOCK_LSP_LOG='{}' exec python3 '{}'\n",
                ts_log.display(),
                mock.display()
            ),
        );
        let node = scratch.join("node");
        executable(
            &node,
            &format!(
                "#!/bin/sh\nMOCK_LSP_TAG=vue MOCK_LSP_ASKS_TSSERVER=1 MOCK_LSP_LOG='{}' exec python3 '{}'\n",
                vue_log.display(),
                mock.display()
            ),
        );
        let data = db::testing::dir("ws-vue-data");
        let installed = data.join("extensions/zed/vue");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        std::fs::create_dir_all(installed.join("grammars")).unwrap();
        for file in ["extension.toml", "extension.wasm"] {
            std::fs::copy(
                fixtures.join("extension/tests/fixtures/vue").join(file),
                installed.join(file),
            )
            .unwrap();
        }
        write_file(
            &installed.join("languages/vue/config.toml"),
            "name = \"Vue.js\"\ngrammar = \"vue\"\npath_suffixes = [\"vue\"]\n",
        );
        for query in ["highlights.scm", "injections.scm"] {
            std::fs::copy(
                fixtures.join("syntax/tests/fixtures/vue").join(query),
                installed.join("languages/vue").join(query),
            )
            .unwrap();
        }
        std::fs::copy(
            fixtures.join("syntax/tests/fixtures/vue/vue.wasm"),
            installed.join("grammars/vue.wasm"),
        )
        .unwrap();
        cx.executor().allow_parking();
        let store = cx.update(|cx| {
            let store = cx.new(|cx| {
                let mut store =
                    ExtensionStore::new(data.join("extensions"), data.join("config"), cx);
                store.world = Some(std::sync::Arc::new(VueWorld {
                    node: node.to_string_lossy().into_owned(),
                    settings: store.settings_for(),
                }));
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
                "typescript-language-server".into(),
                settings::ServerOverride {
                    command: Some(typescript.display().to_string()),
                    args: Some(Vec::new()),
                    ..Default::default()
                },
            );
            // What the user wants of Vue's server, under the name its
            // extension asks by.
            settings.language_servers.insert(
                "vue".into(),
                settings::ServerOverride {
                    initialization_options: Some(
                        serde_json::json!({ "typescript": { "tsdk": "/opt/typescript/lib" } }),
                    ),
                    settings: Some(serde_json::json!({ "vue.inlayHints.missingProps": false })),
                    ..Default::default()
                },
            );
            cx.set_global(settings);
        });
        wait_for(cx, "the extensions folder", &|cx| store.read(cx).loaded);
        let lsp = cx.read(|cx| LspStore::global(cx).unwrap());

        // The TypeScript file first: its server starts as it always did,
        // with nothing added.
        ws.update_in(cx, |w, window, cx| {
            w.open_path(script.clone(), None, window, cx)
        });
        wait_for(cx, "the TypeScript server", &|_| {
            noted(&ts_log, "open").len() == 1
        });
        assert_eq!(noted(&ts_log, "initialize"), [serde_json::Value::Null]);

        // Then the component. Vue's extension is asked what it adds to the
        // TypeScript server; it adds its plugin, so that server is started
        // again with it, and both files are opened in the new one: the
        // component under the name the plugin was told to expect.
        ws.update_in(cx, |w, window, cx| {
            w.open_path(component.clone(), None, window, cx)
        });
        wait_for(cx, "the TypeScript server to start again", &|_| {
            noted(&ts_log, "initialize").len() == 2 && noted(&ts_log, "open").len() == 4
        });
        let options = noted(&ts_log, "initialize")[1].clone();
        assert_eq!(options["plugins"][0]["name"], "@vue/typescript-plugin");
        let work = data.join("extensions/work/vue").canonicalize().unwrap();
        assert_eq!(options["plugins"][0]["location"], work.to_str().unwrap());
        // The server that ran got the component as soon as it was opened;
        // the one started in its place gets both files again.
        let opened = noted(&ts_log, "open");
        assert_eq!(opened[..2], ["typescript", "vue.js"]);
        let mut again = opened[2..].to_vec();
        again.sort_by_key(|id| id.to_string());
        assert_eq!(again, ["typescript", "vue.js"]);

        // The component has two servers: Vue's own, which hears it as
        // `vue`, and the TypeScript one.
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        wait_for(cx, "both servers of the component", &|cx| {
            lsp.read(cx)
                .all_capabilities(editor.read(cx).document())
                .len()
                == 2
        });
        assert_eq!(noted(&vue_log, "open"), ["vue"]);
        // The user's settings reached it through its extension, which read
        // them in place of its own defaults.
        assert_eq!(
            noted(&vue_log, "initialize"),
            [serde_json::json!({ "typescript": { "tsdk": "/opt/typescript/lib" } })]
        );
        wait_for(cx, "the settings to reach Vue's server", &|_| {
            !noted(&vue_log, "configuration").is_empty()
        });
        assert_eq!(
            noted(&vue_log, "configuration"),
            [serde_json::json!({ "vue.inlayHints.missingProps": false })]
        );

        // Vue's server has the TypeScript server asked something through
        // the editor each time the file changes; the answer goes back.
        editor.update_in(cx, |e, _, cx| {
            let end = e.text(cx).len();
            e.select_range(end..end, cx)
        });
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx)));
        cx.simulate_input("pri");
        wait_for(cx, "the TypeScript server's answer to reach Vue's", &|_| {
            noted(&vue_log, "tsserver")
                .iter()
                .any(|answer| answer[0][1]["asked"] == "_vue:projectInfo")
        });
        let answers = noted(&vue_log, "tsserver");
        let answered = answers
            .iter()
            .find(|answer| !answer[0][1].is_null())
            .unwrap();
        assert_eq!(answered[0][0], 7);

        // And completions in the component come from both.
        wait_for(cx, "completions of both", &|cx| {
            editor.read(cx).completion.as_ref().is_some_and(|menu| {
                let has = |label: &str| menu.items.iter().any(|item| item.label == label);
                has("println") && has("vue_println")
            })
        });
        // Vue's extension paints what its server answered: a property as a
        // tag, followed by its detail. The typed word is matched against
        // the name alone. What the TypeScript server answered is not
        // Vue's to paint and stays as it came.
        let labels = cx.read(|cx| {
            let menu = editor.read(cx).completion.as_ref().unwrap();
            let label = |name: &str| {
                let index = menu.items.iter().position(|item| item.label == name)?;
                menu.labels[index].clone()
            };
            (label("vue_title"), label("println"))
        });
        let painted = labels.0.expect("a label painted by Vue's extension");
        assert_eq!(painted.text, "vue_title a prop");
        assert_eq!(painted.runs, [(0..9, syntax::HighlightKind::Tag)]);
        assert_eq!(painted.filter, 0..9);
        assert_eq!(labels.1, None);
    }

    #[gpui::test]
    fn what_an_extension_may_do_is_taken_back_and_what_it_did_is_shown(cx: &mut TestAppContext) {
        // Zed's real Vue extension, installed. Its code gets its server
        // from npm, which in this world makes the files it then looks for.
        let root = db::testing::dir("ws-gate").canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "plain\n").unwrap();
        let data = db::testing::dir("ws-gate-data");
        let installed = data.join("extensions/zed/vue");
        std::fs::create_dir_all(&installed).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../extension/tests/fixtures/vue");
        for file in ["extension.toml", "extension.wasm"] {
            std::fs::copy(fixtures.join(file), installed.join(file)).unwrap();
        }
        // The catalogs are a server that has nothing: the tab is opened
        // below, and no test goes to the real ones.
        let (base, _) = serve(Vec::new());
        cx.executor().allow_parking();
        let store = cx.update(|cx| {
            let store = cx.new(|cx| {
                let mut store =
                    ExtensionStore::new(data.join("extensions"), data.join("config"), cx);
                store.zed_url = base.clone();
                store.open_vsx_url = base.clone();
                store.world = Some(std::sync::Arc::new(VueWorld {
                    node: "node".into(),
                    settings: store.settings_for(),
                }));
                store
            });
            ExtensionStore::set_global(store.clone(), cx);
            store.update(cx, |s, cx| s.scan(cx));
            store
        });
        let (_ws, cx) = setup(cx, root.clone());
        wait_for(cx, "the extensions folder", &|cx| store.read(cx).loaded);
        let server = cx.read(|cx| store.read(cx).servers_for("Vue.js")[0].clone());
        let resolve = |cx: &mut VisualTestContext| {
            let asked = store.update(cx, |store, cx| store.resolve(&server, &root, cx));
            cx.executor().block(asked)
        };
        let did = |cx: &mut VisualTestContext| cx.read(|cx| store.read(cx).did("vue"));
        let npm = extension::Event::Installed("@vue/language-server".into());
        let refused = extension::Event::Refused("Install @vue/language-server from npm".into());

        // Allowed everything, as installing it did: it installs its server,
        // and that is written down.
        assert!(did(cx).is_empty());
        resolve(cx).unwrap();
        assert!(did(cx).contains(&npm), "{:?}", did(cx));
        assert!(!did(cx).contains(&refused));

        // npm is taken back with the button in its details.
        cx.dispatch_action(ShowExtensions);
        let button = bounds_soon(cx, "extension-may-npm-0");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        wait_for(cx, "npm to be refused", &|cx| {
            store.read(cx).refusals("vue").npm
        });
        wait_for(cx, "the decision on disk", &|_| {
            std::fs::read_to_string(data.join("extensions/state.json"))
                .is_ok_and(|text| text.contains("\"npm\": true"))
        });
        // What it has running goes on. Started afresh (off and on again),
        // it asks npm once more, is refused and says why; nothing else was
        // taken from it.
        resolve(cx).unwrap();
        store.update(cx, |store, cx| {
            store.set_off(Origin::Zed, "vue", true, cx);
            store.set_off(Origin::Zed, "vue", false, cx);
        });
        let error = resolve(cx).unwrap_err();
        assert!(error.contains("refused for this extension"), "{error}");
        assert!(did(cx).contains(&refused), "{:?}", did(cx));
        // So is the server that could not be got ready for it.
        assert!(
            did(cx).iter().any(|event| matches!(event,
                extension::Event::Failed(what) if what.starts_with("vue-language-server: "))),
            "{:?}",
            did(cx)
        );
        // What it did before is still there to read.
        assert!(did(cx).contains(&npm));

        // Given back with the same button, it works again, and the file
        // has no trace of the refusal.
        cx.run_until_parked();
        let button = bounds_soon(cx, "extension-may-npm-0");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        wait_for(cx, "npm to be allowed", &|cx| {
            !store.read(cx).refusals("vue").npm
        });
        resolve(cx).unwrap();
        wait_for(cx, "the decision on disk", &|_| {
            std::fs::read_to_string(data.join("extensions/state.json"))
                .is_ok_and(|text| !text.contains("refused"))
        });
    }

    #[gpui::test]
    fn a_language_starts_the_servers_chosen_for_it(cx: &mut TestAppContext) {
        let _languages = extension_languages();
        // Zed's real Ruby extension, which lists seven servers for Ruby.
        // (Its grammar here is Vue's: the test needs a language, not its
        // colors.) None of them is on this machine, and none is needed:
        // what is checked is which of them the file is given to.
        let root = db::testing::dir("ws-ruby-servers").canonicalize().unwrap();
        let app = root.join("app.rb");
        std::fs::write(&app, "puts 1\n").unwrap();
        let data = db::testing::dir("ws-ruby-servers-data");
        let installed = data.join("extensions/zed/ruby");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        std::fs::create_dir_all(installed.join("grammars")).unwrap();
        for file in ["extension.toml", "extension.wasm"] {
            std::fs::copy(
                fixtures.join("extension/tests/fixtures/ruby").join(file),
                installed.join(file),
            )
            .unwrap();
        }
        std::fs::copy(
            fixtures.join("syntax/tests/fixtures/vue/vue.wasm"),
            installed.join("grammars/vue.wasm"),
        )
        .unwrap();
        write_file(
            &installed.join("languages/ruby/config.toml"),
            "name = \"Ruby\"\ngrammar = \"vue\"\npath_suffixes = [\"rb\"]\n",
        );
        cx.executor().allow_parking();
        let extensions = cx.update(|cx| {
            let extensions = cx.new(|cx| {
                let mut store =
                    ExtensionStore::new(data.join("extensions"), data.join("config"), cx);
                store.world = Some(std::sync::Arc::new(ServerOnPath("nothing", String::new())));
                store
            });
            ExtensionStore::set_global(extensions.clone(), cx);
            extensions.update(cx, |s, cx| s.scan(cx));
            extensions
        });
        let (ws, cx) = setup(cx, root.clone());
        wait_for(cx, "the extensions folder", &|cx| {
            extensions.read(cx).loaded
        });
        ws.update_in(cx, |w, window, cx| {
            w.open_path(app.clone(), None, window, cx)
        });
        let servers = |cx: &App| -> Vec<&'static str> {
            let Some(editor) = ws.read(cx).active_editor() else {
                return Vec::new();
            };
            let document = editor.read(cx).document().entity_id();
            LspStore::global(cx)
                .map(|store| store.read(cx).servers_of(document))
                .unwrap_or_default()
        };
        // With nothing said, the one Zed starts for Ruby, of the seven.
        wait_for(cx, "Ruby's server", &|cx| servers(cx) == ["solargraph"]);

        // The user's choice for the language, as Zed writes it: these
        // two in this order, and the open file goes to them at once.
        let choose = |cx: &mut VisualTestContext, names: Option<&[&str]>| {
            cx.update(|_, cx| {
                let mut settings = Settings::get(cx).clone();
                settings.languages.insert(
                    "Ruby".into(),
                    settings::LanguageSettings {
                        language_servers: names
                            .map(|names| names.iter().map(|name| name.to_string()).collect()),
                    },
                );
                cx.set_global(settings);
            });
            cx.run_until_parked();
        };
        choose(cx, Some(&["ruby-lsp", "rubocop"]));
        assert_eq!(cx.read(|cx| servers(cx)), ["ruby-lsp", "rubocop"]);
        // One left out of all the rest.
        choose(cx, Some(&["...", "!solargraph", "!sorbet"]));
        assert_eq!(
            cx.read(|cx| servers(cx)),
            [
                "fuzzy-ruby-server",
                "kanayago",
                "rubocop",
                "ruby-lsp",
                "steep"
            ]
        );
        // A server turned off by its own name stays off whatever the
        // language says.
        cx.update(|_, cx| {
            let mut settings = Settings::get(cx).clone();
            settings.language_servers.insert(
                "rubocop".into(),
                settings::ServerOverride {
                    disabled: true,
                    ..Default::default()
                },
            );
            cx.set_global(settings);
        });
        cx.run_until_parked();
        assert!(!cx.read(|cx| servers(cx)).contains(&"rubocop"));
        // Nothing said again: Zed's choice.
        choose(cx, None);
        assert_eq!(cx.read(|cx| servers(cx)), ["solargraph"]);
    }

    #[gpui::test]
    fn an_extension_paints_the_symbols_its_server_lists(cx: &mut TestAppContext) {
        use crate::symbols::Symbols;
        let _languages = extension_languages();
        // Zed's real Ruby extension, with `solargraph` "on the PATH" as a
        // script that starts the stand-in server. (The grammar is Vue's:
        // the test needs a language, not its colors.)
        let root = db::testing::dir("ws-ruby-symbols").canonicalize().unwrap();
        let app = root.join("app.rb");
        std::fs::write(&app, "fn total\nfn tax\n").unwrap();
        let data = db::testing::dir("ws-ruby-symbols-data");
        let installed = data.join("extensions/zed/ruby");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        std::fs::create_dir_all(installed.join("grammars")).unwrap();
        for file in ["extension.toml", "extension.wasm"] {
            std::fs::copy(
                fixtures.join("extension/tests/fixtures/ruby").join(file),
                installed.join(file),
            )
            .unwrap();
        }
        std::fs::copy(
            fixtures.join("syntax/tests/fixtures/vue/vue.wasm"),
            installed.join("grammars/vue.wasm"),
        )
        .unwrap();
        write_file(
            &installed.join("languages/ruby/config.toml"),
            "name = \"Ruby\"\ngrammar = \"vue\"\npath_suffixes = [\"rb\"]\n",
        );
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_lsp.py");
        let solargraph = db::testing::dir("ws-ruby-symbols-bin").join("solargraph");
        executable(
            &solargraph,
            &format!("#!/bin/sh\nexec python3 '{}'\n", mock.display()),
        );
        cx.executor().allow_parking();
        let extensions = cx.update(|cx| {
            let extensions = cx.new(|cx| {
                let mut store =
                    ExtensionStore::new(data.join("extensions"), data.join("config"), cx);
                store.world = Some(std::sync::Arc::new(ServerOnPath(
                    "solargraph",
                    solargraph.to_string_lossy().into_owned(),
                )));
                store
            });
            ExtensionStore::set_global(extensions.clone(), cx);
            extensions.update(cx, |s, cx| s.scan(cx));
            extensions
        });
        let (ws, cx) = setup(cx, root.clone());
        wait_for(cx, "the extensions folder", &|cx| {
            extensions.read(cx).loaded
        });
        ws.update_in(cx, |w, window, cx| {
            w.open_path(app.clone(), None, window, cx)
        });
        let document = |cx: &App| {
            ws.read(cx)
                .active_editor()
                .map(|e| e.read(cx).document().clone())
        };
        // This includes compiling the real Ruby component, which is much
        // larger than the other fixtures and takes longer on CI runners.
        wait_for_with_timeout(
            cx,
            "the extension's server",
            Duration::from_secs(30),
            &|cx| {
                document(cx).is_some_and(|document| {
                    LspStore::global(cx)
                        .is_some_and(|store| store.read(cx).document_symbols(&document).is_some())
                })
            },
        );

        // The server lists a module and two functions. The extension has
        // a way to show a module (its name, colored as one is where it
        // is declared) and none for these functions, which stay as the
        // server named them.
        cx.simulate_keystrokes("secondary-shift-o");
        let shown = |cx: &mut VisualTestContext| -> Vec<(String, String, Option<String>)> {
            cx.read(|cx| {
                let modal = ws.read(cx).modal.as_ref()?;
                let picker = modal.view.clone().downcast::<Picker<Symbols>>().ok()?;
                Some(picker.read(cx).delegate.shown())
            })
            .unwrap_or_default()
        };
        for _ in 0..400 {
            cx.run_until_parked();
            if shown(cx).first().is_some_and(|row| row.2.is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let rows = shown(cx);
        let names: Vec<&str> = rows.iter().map(|row| row.0.as_str()).collect();
        assert_eq!(names, ["crate", "total", "tax"]);
        assert_eq!(rows[0].2.as_deref(), Some("crate"));
        assert_eq!((rows[1].2.as_deref(), rows[2].2.as_deref()), (None, None));
        // The name is still what is typed to find it.
        cx.simulate_input("crat");
        cx.run_until_parked();
        assert_eq!(shown(cx).len(), 1);
        assert_eq!(shown(cx)[0].2.as_deref(), Some("crate"));
    }

    #[gpui::test]
    fn a_debugger_a_vscode_extension_declares_debugs_a_file(cx: &mut TestAppContext) {
        // A VS Code extension whose manifest says what its debug adapter
        // is: a script and what runs it. None of its code is needed.
        let root = db::testing::dir("ws-vsx-debug").canonicalize().unwrap();
        let app = root.join("app.demo");
        std::fs::write(&app, "one\ntwo\nthree\n").unwrap();
        let data = db::testing::dir("ws-vsx-debug-data");
        let log = data.join("launch.json");
        let installed = data.join("extensions/vscode/acme.demo-debug");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_dap_stdio.py"),
            installed.join("adapter.py"),
        )
        .unwrap();
        let manifest = serde_json::json!({
            "name": "demo-debug", "publisher": "Acme", "version": "1.0.0",
            "contributes": {
                "languages": [{ "id": "demo", "extensions": [".demo"] }],
                "breakpoints": [{ "language": "demo" }],
                "debuggers": [
                    {
                        "type": "demo", "label": "Demo Debug",
                        "runtime": "python3", "program": "./adapter.py", "args": [log],
                        "initialConfigurations": [
                            { "type": "demo", "request": "attach", "name": "Attach" },
                            {
                                "type": "demo", "request": "launch", "name": "Launch",
                                "program": "${workspaceFolder}/${command:AskForProgramName}",
                                "stopOnEntry": true, "trace": ["${fileBasename}"],
                            },
                        ],
                    },
                    // One whose adapter only its code knows how to start.
                    { "type": "coded", "label": "Coded" },
                ],
            },
        });
        std::fs::write(installed.join("package.json"), manifest.to_string()).unwrap();
        cx.executor().allow_parking();
        let (extensions, debug) = cx.update(|cx| {
            let extensions =
                cx.new(|cx| ExtensionStore::new(data.join("extensions"), data.join("config"), cx));
            ExtensionStore::set_global(extensions.clone(), cx);
            extensions.update(cx, |s, cx| s.scan(cx));
            let debug = cx.new(|_| {
                crate::debug::DebugStore::new(
                    data.join("debug"),
                    crate::debug::AdapterSpec::JsDebug,
                )
            });
            crate::debug::DebugStore::set_global(debug.clone(), cx);
            (extensions, debug)
        });
        let (ws, cx) = setup(cx, root.clone());
        wait_for(cx, "the extensions folder", &|cx| {
            extensions.read(cx).loaded
        });
        let found = cx.read(|cx| {
            extensions
                .read(cx)
                .find(Origin::VsCode, "Acme.demo-debug")
                .cloned()
        });
        let found = found.unwrap();
        assert_eq!(found.debuggers.len(), 1);
        assert_eq!(found.debuggers[0].languages, ["demo"]);
        assert!(
            found
                .missing
                .contains(&"A debugger its code starts".to_string())
        );
        assert_eq!(found.provides(), "1 debugger");

        // The file has no language here, and is debugged all the same:
        // the debugger says which files are its own.
        ws.update_in(cx, |w, window, cx| {
            w.open_path(app.clone(), None, window, cx)
        });
        let configs = cx.read(|cx| crate::debug_launch::from_extensions(&root, &app, cx));
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "demo app.demo");
        let other = root.join("notes.txt");
        assert!(cx.read(|cx| crate::debug_launch::from_extensions(&root, &other, cx).is_empty()));

        // Started, the adapter runs as the manifest says and the run
        // stops on the breakpoint in the file.
        debug.update(cx, |s, cx| {
            s.toggle(&app, 2, cx);
            s.start(configs[0].clone(), root.clone(), cx)
        });
        wait_for(cx, "the pause in app.demo", &|cx| {
            paused_line(&debug, cx) == Some(2)
        });
        // It was given the launch the manifest suggests, with its places
        // filled in: the file where VS Code would ask for one.
        let launch: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
        assert_eq!(launch["type"], "demo");
        assert_eq!(launch["request"], "launch");
        assert_eq!(launch["program"], app.display().to_string());
        assert_eq!(launch["cwd"], root.display().to_string());
        assert_eq!(launch["stopOnEntry"], true);
        assert_eq!(launch["trace"][0], "app.demo");
        let requests = std::fs::read_to_string(log.with_extension("requests")).unwrap();
        assert_eq!(
            requests.lines().take(4).collect::<Vec<_>>(),
            [
                "initialize",
                "launch",
                "setBreakpoints",
                "configurationDone"
            ]
        );
        debug.update(cx, |s, cx| s.stop(cx));
        cx.run_until_parked();
        assert!(cx.read(|cx| !debug.read(cx).state.active()));
    }

    #[gpui::test]
    fn a_debug_adapter_of_an_extension_debugs_a_file_of_its_language(cx: &mut TestAppContext) {
        let _languages = extension_languages();
        // Zed's real Ruby extension, installed, with a Ruby language that
        // names `rdbg` as its debugger. (Its grammar here is Vue's: the test
        // needs a language, not its colors.) `rdbg` is "on the PATH" as a
        // script that starts the stand-in adapter on the port it is given.
        let root = db::testing::dir("ws-rdbg").canonicalize().unwrap();
        let app = root.join("app.rb");
        std::fs::write(
            &app,
            "def add(a, b)\n  sum = a + b\n  sum\nend\nputs add(2, 3)\n",
        )
        .unwrap();
        let data = db::testing::dir("ws-rdbg-data");
        let installed = data.join("extensions/zed/ruby");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        std::fs::create_dir_all(installed.join("grammars")).unwrap();
        for file in ["extension.toml", "extension.wasm"] {
            std::fs::copy(
                fixtures.join("extension/tests/fixtures/ruby").join(file),
                installed.join(file),
            )
            .unwrap();
        }
        std::fs::copy(
            fixtures.join("syntax/tests/fixtures/vue/vue.wasm"),
            installed.join("grammars/vue.wasm"),
        )
        .unwrap();
        write_file(
            &installed.join("languages/ruby/config.toml"),
            "name = \"Ruby\"\ngrammar = \"vue\"\npath_suffixes = [\"rb\"]\ndebuggers = [\"rdbg\"]\n",
        );
        let scratch = db::testing::dir("ws-rdbg-bin");
        let rdbg = scratch.join("rdbg");
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_dap.py");
        executable(
            &rdbg,
            &format!(
                "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in --port=*) port=\"${{a#--port=}}\";; esac; done\nexec python3 '{}' \"$port\" 127.0.0.1\n",
                mock.display()
            ),
        );
        cx.executor().allow_parking();
        let (extensions, debug) = cx.update(|cx| {
            let extensions = cx.new(|cx| {
                let mut store =
                    ExtensionStore::new(data.join("extensions"), data.join("config"), cx);
                store.world = Some(std::sync::Arc::new(ServerOnPath(
                    "rdbg",
                    rdbg.to_string_lossy().into_owned(),
                )));
                store
            });
            ExtensionStore::set_global(extensions.clone(), cx);
            extensions.update(cx, |s, cx| s.scan(cx));
            let debug = cx.new(|_| {
                crate::debug::DebugStore::new(
                    data.join("debug"),
                    crate::debug::AdapterSpec::JsDebug,
                )
            });
            crate::debug::DebugStore::set_global(debug.clone(), cx);
            (extensions, debug)
        });
        let (ws, cx) = setup(cx, root.clone());
        wait_for(cx, "the extensions folder", &|cx| {
            extensions.read(cx).loaded
        });
        ws.update_in(cx, |w, window, cx| {
            w.open_path(app.clone(), None, window, cx)
        });
        wait_for(cx, "the language", &|cx| {
            ws.read(cx)
                .active_editor()
                .is_some_and(|e| e.read(cx).doc(cx).language_name() == Some("Ruby"))
        });

        // The extension says it brings the adapter, and the file of its
        // language can be debugged with it; a file of another cannot.
        assert_eq!(
            cx.read(|cx| extensions
                .read(cx)
                .find(Origin::Zed, "ruby")
                .unwrap()
                .provides()),
            "1 language, 8 language servers, 1 debug adapter"
        );
        let configs = cx.read(|cx| crate::debug_launch::from_extensions(&root, &app, cx));
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "rdbg app.rb");
        let none =
            cx.read(|cx| crate::debug_launch::from_extensions(&root, &root.join("a.js"), cx));
        assert!(none.is_empty());

        // Started, the extension is asked how; the adapter it names is
        // started as it says and listens where the editor told it to. The
        // run stops on the breakpoint in the file.
        debug.update(cx, |s, cx| {
            s.toggle(&app, 2, cx);
            s.start(configs[0].clone(), root.clone(), cx)
        });
        wait_for(cx, "the pause in app.rb", &|cx| {
            paused_line(&debug, cx) == Some(2)
        });
        assert!(cx.read(|cx| debug.read(cx).state.active()));
        debug.update(cx, |s, cx| s.stop(cx));
        cx.run_until_parked();
        assert!(cx.read(|cx| !debug.read(cx).state.active()));

        // Turned off, the extension debugs nothing.
        extensions.update(cx, |s, cx| s.set_off(Origin::Zed, "ruby", true, cx));
        let none = cx.read(|cx| extensions.read(cx).debuggers_for("Ruby"));
        assert!(none.is_empty());
    }

    #[gpui::test]
    fn panels_are_in_the_docks_the_layout_puts_them_in(cx: &mut TestAppContext) {
        let root = db::testing::dir("ws-docks").canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "plain\n").unwrap();
        let config = db::testing::dir("ws-docks-config");
        let (ws, cx) = setup(cx, root.clone());
        let docks = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.read(|cx| (ws.read(cx).left, ws.read(cx).right))
        };
        let layout = |cx: &mut VisualTestContext, text: &str| {
            std::fs::write(config.join("layout.json"), text).unwrap();
            cx.update(|_, cx| settings::reload_from(&config, cx));
            assert!(cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));
        };
        // As it comes: Files on the left, the right dock closed; the chat
        // opens on the right and the agent takes its place there.
        assert_eq!(docks(cx), (Some(Panel::Files), None));
        cx.dispatch_action(ToggleChat);
        assert_eq!(docks(cx), (Some(Panel::Files), Some(Panel::Chat)));
        cx.dispatch_action(ShowAgent);
        assert_eq!(docks(cx), (Some(Panel::Files), Some(Panel::Agent)));
        cx.dispatch_action(ShowSearch);
        assert_eq!(docks(cx), (Some(Panel::Search), Some(Panel::Agent)));

        // The layout moves the agent and the chat to the left, Search and
        // Files to the right, and hides API. What was shown goes with its
        // panel: the agent is on the left now and Search on the right.
        layout(
            cx,
            r#"{
              "left": { "panels": ["agent", "chat", "services"] },
              "right": { "panels": ["search", "files"] },
              "hidden": ["api"]
            }"#,
        );
        assert_eq!(docks(cx), (Some(Panel::Agent), Some(Panel::Search)));
        // Each dock has a tab for each of its panels, in the file's order.
        let tab = |cx: &mut VisualTestContext, panel: &'static str| bounds_soon(cx, panel);
        let dock_left = bounds_soon(cx, "dock-left");
        let dock_right = bounds_soon(cx, "dock-right");
        let (agent, chat) = (tab(cx, "panel-agent"), tab(cx, "panel-chat"));
        assert!(dock_left.contains(&agent.center()) && dock_left.contains(&chat.center()));
        assert!(agent.left() < chat.left());
        let (search, files) = (tab(cx, "panel-search"), tab(cx, "panel-files"));
        assert!(dock_right.contains(&search.center()) && dock_right.contains(&files.center()));
        assert!(search.left() < files.left());

        // Commands and tabs open a panel where it is now.
        cx.dispatch_action(ShowFiles);
        assert_eq!(docks(cx), (Some(Panel::Agent), Some(Panel::Files)));
        cx.simulate_click(chat.center(), gpui::Modifiers::default());
        assert_eq!(docks(cx), (Some(Panel::Chat), Some(Panel::Files)));
        // The chat's key closes the dock the chat is in, which is the
        // left one, and opens it there again.
        cx.dispatch_action(ToggleChat);
        assert_eq!(docks(cx), (None, Some(Panel::Files)));
        cx.dispatch_action(ToggleChat);
        assert_eq!(docks(cx), (Some(Panel::Chat), Some(Panel::Files)));
        // The sidebar's key closes the left dock and opens it on the
        // first panel it has.
        cx.dispatch_action(ToggleSidebar);
        assert_eq!(docks(cx), (None, Some(Panel::Files)));
        cx.dispatch_action(ToggleSidebar);
        assert_eq!(docks(cx), (Some(Panel::Agent), Some(Panel::Files)));
        // A hidden panel has no tab, and its command still opens it, in
        // the dock it comes in, with a tab for as long as it shows.
        assert!(cx.read(|cx| Layout::get(cx).place(Panel::Api).is_none()));
        cx.dispatch_action(ShowApi);
        assert_eq!(docks(cx), (Some(Panel::Api), Some(Panel::Files)));
        let api = tab(cx, "panel-api");
        assert!(dock_left.contains(&api.center()));

        // Back to the layout it came with: Files returns to the left,
        // where API is shown and stays; the right dock, which lost what it
        // showed, shows the first panel it has.
        layout(cx, "{}");
        assert_eq!(docks(cx), (Some(Panel::Api), Some(Panel::Chat)));
        cx.dispatch_action(ShowFiles);
        cx.dispatch_action(ShowAgent);
        assert_eq!(docks(cx), (Some(Panel::Files), Some(Panel::Agent)));
    }

    #[gpui::test]
    fn terminals_and_results_are_panels_of_any_dock_too(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let root = db::testing::dir("ws-docks-low").canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "plain\n").unwrap();
        let config = db::testing::dir("ws-docks-low-config");
        let (ws, cx) = setup(cx, root.clone());
        let docks = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.read(|cx| {
                let ws = ws.read(cx);
                (ws.left, ws.right, ws.bottom)
            })
        };
        // Where a tab is once the window has drawn it inside `dock`: a
        // tab that moved is still remembered where it was last.
        let inside = |cx: &mut VisualTestContext, dock: &'static str, tab: &'static str| {
            for _ in 0..200 {
                cx.update(|window, _| window.refresh());
                cx.run_until_parked();
                if let (Some(dock), Some(tab)) = (cx.debug_bounds(dock), cx.debug_bounds(tab))
                    && dock.contains(&tab.center())
                {
                    return tab;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("no {tab} in {dock}");
        };
        // As it comes, the bottom dock is closed until something is in
        // it: a terminal opens it, and results come in front there.
        assert_eq!(docks(cx), (Some(Panel::Files), None, None));
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
        assert_eq!(docks(cx).2, Some(Panel::Terminal));
        ws.update_in(cx, |w, window, cx| {
            w.show_results = true;
            w.show_panel(Panel::Results, cx);
            window.focus(&w.results.focus_handle(cx));
        });
        assert_eq!(docks(cx).2, Some(Panel::Results));
        inside(cx, "dock-bottom", "terminal-new");
        inside(cx, "dock-bottom", "results-tab");

        // The layout puts the terminals on the right, alone, results on
        // the left and Search at the bottom. Files stays in front on the
        // left; the bottom dock lost what it showed and shows Search. The
        // keyboard was in the results, which no dock shows now, and is
        // not left there.
        std::fs::write(
            config.join("layout.json"),
            r#"{
              "left": { "panels": ["files", "results", "chat", "agent"] },
              "right": { "panels": ["terminal"] },
              "bottom": { "panels": ["search"] }
            }"#,
        )
        .unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        assert!(cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));
        assert_eq!(docks(cx), (Some(Panel::Files), None, Some(Panel::Search)));
        inside(cx, "dock-bottom", "panel-search");
        assert!(cx.update(|window, cx| {
            !ws.read(cx)
                .results
                .focus_handle(cx)
                .contains_focused(window, cx)
        }));

        // The terminal's key opens the dock the terminals are in now, and
        // their tabs are there.
        cx.dispatch_action(ToggleTerminal);
        assert_eq!(
            docks(cx),
            (
                Some(Panel::Files),
                Some(Panel::Terminal),
                Some(Panel::Search)
            )
        );
        inside(cx, "dock-right", "terminal-new");
        // The tab of the results is on the left, and shows them there.
        let results = inside(cx, "dock-left", "results-tab");
        cx.simulate_click(results.center(), gpui::Modifiers::default());
        assert_eq!(docks(cx).0, Some(Panel::Results));
        // Closed, they leave the dock to the first panel it has.
        ws.update_in(cx, |w, window, cx| w.close_results(window, cx));
        assert_eq!(docks(cx).0, Some(Panel::Files));
        assert!(cx.read(|cx| !ws.read(cx).available(Panel::Results)));

        // The key again, with the keyboard in the terminal, closes that
        // dock; once more opens it. The last terminal to end closes it,
        // since the dock has nothing else, and the keyboard is not left
        // in it.
        ws.update_in(cx, |_, window, cx| window.focus(&terminal.focus_handle(cx)));
        cx.dispatch_action(ToggleTerminal);
        assert_eq!(docks(cx).1, None);
        cx.dispatch_action(ToggleTerminal);
        assert_eq!(docks(cx).1, Some(Panel::Terminal));
        cx.simulate_input("exit");
        cx.simulate_keystrokes("enter");
        wait_for(cx, "the terminal to end", &|cx| {
            ws.read(cx).terminals.is_empty()
        });
        assert_eq!(docks(cx), (Some(Panel::Files), None, Some(Panel::Search)));
        cx.dispatch_action(ShowAgent);
        assert_eq!(docks(cx).0, Some(Panel::Agent));
    }

    #[gpui::test]
    fn a_theme_file_is_applied_as_it_is_saved(cx: &mut TestAppContext) {
        use gpui::{Hsla, rgb};
        cx.executor().allow_parking();
        let root = db::testing::dir("ws-theme-live").canonicalize().unwrap();
        let config = db::testing::dir("ws-theme-live-config")
            .canonicalize()
            .unwrap();
        let theme_file = |bg: &str| {
            format!(
                r##"{{ "name": "Mine", "appearance": "dark",
                      "colors": {{ "bg": "{bg}", "accent": "#112233" }},
                      "terminal": {{ "red": "#ff0000" }},
                      "shapes": {{ "control_radius": 3, "border_width": 2 }} }}"##
            )
        };
        std::fs::create_dir_all(config.join("themes")).unwrap();
        std::fs::write(config.join("themes/mine.json"), theme_file("#101010")).unwrap();
        std::fs::write(
            config.join("settings.json"),
            r##"{ "theme": "Mine", "theme_overrides": { "accent": "#00ff88" } }"##,
        )
        .unwrap();
        let (_ws, cx) = setup(cx, root);
        cx.update(|_, cx| {
            settings::reload_from(&config, cx);
            settings::watch_dir(config.clone(), cx);
        });
        cx.run_until_parked();
        // The theme of the file, with the token of the settings over it.
        let theme = |cx: &App| cx.global::<crate::theme::Theme>().clone();
        assert_eq!(cx.read(theme).bg, Hsla::from(rgb(0x101010)));
        assert_eq!(cx.read(theme).terminal[1], Hsla::from(rgb(0xff0000)));
        assert_eq!(cx.read(theme).accent, Hsla::from(rgb(0x00ff88)));
        // Its shapes too; the ones it leaves out are the editor's.
        let shape = cx.read(theme).shape;
        assert_eq!((shape.control, shape.border), (px(3.), px(2.)));
        assert_eq!(shape.token, px(6.));
        assert_eq!(cx.read(theme).fg, crate::theme::Theme::dark().fg);

        // The theme's file is saved with another background: the window
        // has it without anything else being touched.
        std::fs::write(config.join("themes/mine.json"), theme_file("#202020")).unwrap();
        wait_for(cx, "the saved theme", &|cx| {
            theme(cx).bg == Hsla::from(rgb(0x202020))
        });
        assert_eq!(cx.read(theme).accent, Hsla::from(rgb(0x00ff88)));
        // And the settings, with the token taken back: the theme's own.
        std::fs::write(config.join("settings.json"), r#"{ "theme": "Mine" }"#).unwrap();
        wait_for(cx, "the theme's own accent", &|cx| {
            theme(cx).accent == Hsla::from(rgb(0x112233))
        });
    }

    #[gpui::test]
    fn the_interface_has_the_font_size_and_density_of_the_settings(cx: &mut TestAppContext) {
        let root = db::testing::dir("ws-ui-font").canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "plain\n").unwrap();
        let config = db::testing::dir("ws-ui-font-config");
        let (ws, cx) = setup(cx, root.clone());
        wait_for(cx, "the tree", &|cx| {
            !ws.read(cx).project.read(cx).is_scanning()
        });
        let settings = |cx: &mut VisualTestContext, text: &str| {
            std::fs::write(config.join("settings.json"), text).unwrap();
            cx.update(|_, cx| settings::reload_from(&config, cx));
            assert!(cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));
        };
        // The height of a row of the file tree as it is drawn now, and
        // what a rem is in the window.
        let row = |cx: &mut VisualTestContext| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            let row = cx.debug_bounds("file-row-0").expect("a row of the tree");
            (
                f32::from(row.size.height),
                cx.update(|window, _| window.rem_size()),
            )
        };
        // As it comes.
        assert_eq!(row(cx), (24., px(16.)));
        // Rows lower and taller by the density; the text is the same.
        settings(cx, r#"{ "ui_density": "compact" }"#);
        assert_eq!(row(cx), (20., px(16.)));
        settings(cx, r#"{ "ui_density": "comfortable" }"#);
        assert_eq!(row(cx), (30., px(16.)));
        // Larger text: everything sized in rems grows, and rows with it,
        // so that a line still fits.
        settings(cx, r#"{ "ui_font_size": 15 }"#);
        assert_eq!(row(cx), (29., px(19.2)));
        // Back as it came when the file says nothing.
        settings(cx, "{}");
        assert_eq!(row(cx), (24., px(16.)));

        // A theme that leaves more room around things: gaps and paddings
        // grow, which is the rem, and the text stays the size it was.
        let inset = |cx: &mut VisualTestContext| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            let dock = cx.debug_bounds("dock-left").unwrap();
            let tab = cx.debug_bounds("panel-files").unwrap();
            f32::from(tab.left() - dock.left())
        };
        let text = |cx: &mut VisualTestContext| {
            cx.update(|window, _| {
                gpui::Rems::from(crate::theme::UI_FONT_SIZE).to_pixels(window.rem_size())
            })
        };
        let (tight, written) = (inset(cx), text(cx));
        assert_eq!(written, px(12.5));
        settings(
            cx,
            r#"{ "theme_overrides": { "shapes": { "spacing": 1.5 } } }"#,
        );
        assert_eq!(row(cx), (24., px(24.)));
        assert_eq!(inset(cx), tight * 1.5);
        assert_eq!(text(cx), px(12.5));
        // With larger text as well: the two multiply, and the text is
        // only as large as it was asked to be.
        settings(
            cx,
            r#"{ "ui_font_size": 15, "theme_overrides": { "shapes": { "spacing": 1.5 } } }"#,
        );
        let near = |size: gpui::Pixels, wanted: f32| (f32::from(size) - wanted).abs() < 0.001;
        assert!(near(cx.update(|window, _| window.rem_size()), 28.8));
        assert!(near(text(cx), 15.));
        settings(cx, "{}");
        assert_eq!((inset(cx), text(cx)), (tight, px(12.5)));
    }

    #[gpui::test]
    fn the_bars_show_the_items_the_layout_names(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let root = git_fixture("ws-bars");
        let config = db::testing::dir("ws-bars-config");
        let (ws, cx) = setup(cx, root.clone());
        ws.update_in(cx, |w, window, cx| {
            w.add_editor(Some(root.join("a.txt")), "one\ntwo\n", None, window, cx)
        });
        wait_for(cx, "the branch", &|cx| {
            ws.read(cx).git.read(cx).status().branch.is_some()
        });
        // Where something is in the window as it is drawn now.
        let at = |cx: &mut VisualTestContext, selector: &'static str| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("no {selector}"))
        };
        let layout = |cx: &mut VisualTestContext, text: &str| {
            std::fs::write(config.join("layout.json"), text).unwrap();
            cx.update(|_, cx| settings::reload_from(&config, cx));
            assert!(cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));
        };
        // Whether an item has something to say now.
        let says = |cx: &mut VisualTestContext, item: Item| {
            ws.update(cx, |w, cx| !w.bar_item(&item, cx).is_empty())
        };

        // As they come: the project's name in the title bar, the cursor,
        // the indent and the language at the left of the status bar, in
        // that order. The tabs are above the file.
        let (title, status) = (at(cx, "title-bar"), at(cx, "status-bar"));
        assert!(title.contains(&at(cx, "item-project").center()));
        let (position, indent) = (at(cx, "item-position"), at(cx, "item-indent"));
        let language = at(cx, "item-language");
        assert!(status.contains(&position.center()) && status.contains(&language.center()));
        assert!(position.left() < indent.left() && indent.left() < language.left());
        let (tabs, body) = (at(cx, "tab-bar-0"), at(cx, "pane-body-0"));
        assert_eq!(tabs.top(), title.bottom());
        assert_eq!(body.top(), tabs.bottom());
        assert_eq!(body.bottom(), status.top());
        // An item with nothing to say is not drawn: no query file is in
        // front, and no file has a problem.
        assert!(!says(cx, Item::Connection) && !says(cx, Item::Problems));
        assert!(says(cx, Item::Branch) && says(cx, Item::File));

        // The file names them. The cursor goes to the right of the title
        // bar, the branch and the file's name come to the status bar,
        // the language to its right end, and the tabs below the file.
        layout(
            cx,
            r#"{
              "title_bar": { "right": ["position"] },
              "status_bar": { "left": ["branch", "file"], "right": ["language"] },
              "tab_bar": { "place": "bottom" }
            }"#,
        );
        let position = at(cx, "item-position");
        assert!(title.contains(&position.center()));
        assert!(position.left() > at(cx, "item-project").right());
        let (branch, file) = (at(cx, "item-branch"), at(cx, "item-file"));
        assert!(status.contains(&branch.center()) && status.contains(&file.center()));
        assert!(branch.left() < file.left());
        assert!(at(cx, "item-language").left() > file.right());
        let (tabs, body) = (at(cx, "tab-bar-0"), at(cx, "pane-body-0"));
        assert_eq!(body.top(), title.bottom());
        assert_eq!(tabs.top(), body.bottom());
        assert_eq!(tabs.bottom(), status.top());
        // The branch opens the list of branches.
        cx.simulate_click(branch.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
        cx.simulate_keystrokes("escape");

        // No tabs at all: the file has the whole height between the bars.
        layout(cx, r#"{ "tab_bar": { "place": "none" } }"#);
        let body = at(cx, "pane-body-0");
        assert_eq!(body.top(), title.bottom());
        assert_eq!(body.bottom(), status.top());
        assert!(cx.read(|cx| Layout::get(cx).status_bar.left().contains(&Item::Position)));
    }

    #[gpui::test]
    fn an_item_is_drawn_as_asked_and_a_button_runs_its_command(cx: &mut TestAppContext) {
        use Item::*;
        use layout::{Command, Display};
        let root = db::testing::dir("ws-bar-buttons").canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "plain\n").unwrap();
        let config = db::testing::dir("ws-bar-buttons-config");
        let file = config.join("layout.json");
        std::fs::write(&file, "// mine\n{}\n").unwrap();
        let (ws, cx) = setup(cx, root.clone());
        ws.update_in(cx, |w, window, cx| {
            w.add_editor(Some(root.join("notes.txt")), "plain\n", None, window, cx)
        });
        cx.update(|_, cx| settings::reload_from(&config, cx));
        let none = gpui::Modifiers::default();
        let at = |cx: &mut VisualTestContext, selector: &'static str| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            bounds_soon(cx, selector).center()
        };
        let click = |cx: &mut VisualTestContext, selector: &'static str| {
            let point = at(cx, selector);
            cx.simulate_click(point, none);
            cx.run_until_parked();
        };
        let menu = |cx: &mut VisualTestContext, selector: &'static str, option: &'static str| {
            let point = at(cx, selector);
            cx.simulate_mouse_down(point, MouseButton::Right, none);
            cx.simulate_mouse_up(point, MouseButton::Right, none);
            cx.run_until_parked();
            click(cx, option);
        };
        let layout = |cx: &mut VisualTestContext| cx.read(|cx| Layout::get(cx).clone());
        let left = |cx: &mut VisualTestContext| cx.read(|cx| ws.read(cx).left);
        let button = Button {
            button: "button-1".into(),
        };

        // The menu of an item says how it is drawn.
        assert_eq!(layout(cx).style(&Position).display, Display::Both);
        menu(cx, "item-position", "bar-menu-icon");
        assert_eq!(layout(cx).style(&Position).display, Display::Icon);
        menu(cx, "item-position", "bar-menu-text");
        assert_eq!(layout(cx).style(&Position).display, Display::Text);

        // And adds a button for a command, chosen from all of them, at
        // the end of the bar the menu was opened on.
        menu(cx, "item-position", "bar-menu-add");
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
        cx.simulate_input("toggle sidebar");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.read(|cx| ws.read(cx).modal.is_none()));
        let made = layout(cx).style(&button).clone();
        assert_eq!(made.label.as_deref(), Some("Toggle sidebar"));
        assert_eq!(
            made.command,
            Some(Command::Action("workspace::ToggleSidebar".into()))
        );
        assert_eq!(layout(cx).item_place(&button), Some(BarEnd::StatusLeft));
        // It runs the command, as an icon alone too.
        assert_eq!(left(cx), Some(Panel::Files));
        click(cx, "item-button-1");
        assert_eq!(left(cx), None);
        menu(cx, "item-button-1", "bar-menu-icon");
        assert_eq!(layout(cx).style(&button).display, Display::Icon);
        click(cx, "item-button-1");
        assert_eq!(left(cx), Some(Panel::Files));
        // All of it is in the file, next to what the user wrote there.
        for _ in 0..200 {
            cx.run_until_parked();
            if std::fs::read_to_string(&file)
                .is_ok_and(|s| layout::parse(&s).is_ok_and(|read| read == layout(cx)))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let saved = std::fs::read_to_string(&file).unwrap();
        assert_eq!(layout::parse(&saved).unwrap(), layout(cx));
        assert!(
            saved.contains("// mine") && saved.contains("button-1"),
            "{saved}"
        );

        // The file gives an item of the editor's own another word and
        // another command: the language now opens Search.
        std::fs::write(
            &file,
            r#"{ "items": { "language": { "label": "Find", "command": "workspace::ShowSearch" } } }"#,
        )
        .unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        assert!(cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));
        click(cx, "item-language");
        assert_eq!(left(cx), Some(Panel::Search));
        // A command that is none is a mistake, and the layout stays.
        let before = layout(cx);
        std::fs::write(
            &file,
            r#"{ "items": { "language": { "command": "workspace::Nope" } } }"#,
        )
        .unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        let errors = cx.read(|cx| cx.global::<settings::ConfigErrors>().0.clone());
        assert!(errors.iter().any(|e| e.contains("language")), "{errors:?}");
        assert_eq!(layout(cx), before);

        // A button is removed from its menu: off the bar and out of the
        // file, not hidden.
        std::fs::write(&file, saved).unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        assert_eq!(layout(cx).item_place(&button), Some(BarEnd::StatusLeft));
        menu(cx, "item-button-1", "bar-menu-remove");
        assert_eq!(layout(cx).item_place(&button), None);
        assert!(!layout(cx).items.contains_key("button-1"));
    }

    #[gpui::test]
    fn bar_items_are_dragged_and_restored_in_the_selected_layout(cx: &mut TestAppContext) {
        use BarEnd::*;
        use Item::*;
        cx.executor().allow_parking();
        let root = git_fixture("ws-bar-hand");
        let config = db::testing::dir("ws-bar-hand-config");
        let common = config.join("layout.json");
        let original = "// default stays here\n{}\n";
        std::fs::write(&common, original).unwrap();
        std::fs::create_dir_all(config.join("layouts")).unwrap();
        let named = config.join("layouts/Review.json");
        std::fs::write(
            &named,
            r#"// review
        {
          "title_bar": { "left": ["project"], "right": [] },
          "status_bar": { "left": ["position", "indent", "language", "problems"], "right": [] },
          "tab_bar": { "place": "bottom" }
        }"#,
        )
        .unwrap();
        let (ws, cx) = setup(cx, root.clone());
        ws.update_in(cx, |w, window, cx| {
            w.add_editor(Some(root.join("a.txt")), "one\ntwo\n", None, window, cx)
        });
        cx.update(|_, cx| settings::reload_from(&config, cx));
        cx.dispatch_action(SwitchLayout {
            name: Some("Review".into()),
        });
        wait_for(cx, "Review selected", &|cx| {
            layout::active(cx).as_deref() == Some("Review")
        });
        wait_for(cx, "the branch", &|cx| {
            ws.read(cx).git.read(cx).status().branch.is_some()
        });
        let none = gpui::Modifiers::default();
        let at = |cx: &mut VisualTestContext, selector: &'static str| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            bounds_soon(cx, selector).center()
        };
        let open = |cx: &mut VisualTestContext, selector: &'static str| {
            let point = at(cx, selector);
            cx.simulate_mouse_down(point, MouseButton::Right, none);
            cx.simulate_mouse_up(point, MouseButton::Right, none);
            cx.run_until_parked();
        };
        let menu = |cx: &mut VisualTestContext, selector: &'static str, option: &'static str| {
            open(cx, selector);
            let point = at(cx, option);
            cx.simulate_click(point, none);
            cx.run_until_parked();
        };
        let drag = |cx: &mut VisualTestContext, from: &'static str, to: &'static str| {
            let from = at(cx, from);
            cx.simulate_mouse_down(from, MouseButton::Left, none);
            cx.simulate_mouse_move(from + gpui::point(px(6.), px(0.)), MouseButton::Left, none);
            let to = at(cx, to);
            cx.simulate_mouse_move(to, MouseButton::Left, none);
            cx.simulate_mouse_up(to, MouseButton::Left, none);
            cx.run_until_parked();
        };
        let layout = |cx: &mut VisualTestContext| cx.read(|cx| Layout::get(cx).clone());

        menu(cx, "item-position", "bar-menu-move-title-right");
        assert_eq!(layout(cx).item_place(Position), Some(TitleRight));
        menu(cx, "item-project", "bar-menu-hide");
        assert_eq!(layout(cx).item_place(Project), None);
        assert!(cx.read(|cx| ws.read(cx).bar_menu.is_none()));
        // The now empty end still has a menu and can bring it back.
        menu(cx, "bar-title-left", "bar-menu-show-project");
        assert_eq!(layout(cx).items(TitleLeft), [Project]);
        menu(cx, "item-language", "bar-menu-move-status-right");
        assert_eq!(layout(cx).items(StatusRight), [Language]);

        drag(cx, "item-project", "item-position");
        assert_eq!(layout(cx).items(TitleRight), [Project, Position]);
        drag(cx, "item-position", "bar-status-left");
        assert_eq!(layout(cx).items(StatusLeft), [Indent, Problems, Position]);
        drag(cx, "item-indent", "bar-title-left");
        assert_eq!(layout(cx).items(TitleLeft), [Indent]);
        drag(cx, "item-language", "item-position");
        assert_eq!(layout(cx).items(StatusLeft), [Problems, Language, Position]);
        assert!(layout(cx).items(StatusRight).is_empty());
        let before = layout(cx);
        drag(cx, "item-project", "item-project");
        assert_eq!(layout(cx), before);
        assert_eq!(layout(cx).item_place(Problems), Some(StatusLeft));
        assert!(ws.update(cx, |w, cx| w.bar_item(&Problems, cx).is_empty()));

        open(cx, "item-indent");
        cx.simulate_keystrokes("escape");
        assert!(cx.read(|cx| ws.read(cx).bar_menu.is_none()));
        open(cx, "bar-status-right");
        let outside = at(cx, "pane-body-0");
        cx.simulate_click(outside, none);
        assert!(cx.read(|cx| ws.read(cx).bar_menu.is_none()));
        // A clickable item keeps its action after moving and does not
        // open its picker while it is being dragged instead.
        menu(cx, "bar-status-right", "bar-menu-show-branch");
        drag(cx, "item-branch", "bar-title-left");
        assert_eq!(layout(cx).items(TitleLeft), [Indent, Branch]);
        drag(cx, "item-branch", "item-indent");
        assert_eq!(layout(cx).items(TitleLeft), [Branch, Indent]);
        let before = layout(cx);
        drag(cx, "item-branch", "pane-body-0");
        assert_eq!(layout(cx), before);
        assert!(cx.read(|cx| ws.read(cx).modal.is_none()));
        let branch = at(cx, "item-branch");
        cx.simulate_click(branch, none);
        assert!(cx.read(|cx| ws.read(cx).modal.is_some()));
        cx.simulate_keystrokes("escape");

        // Both bars were edited quickly. Every write reaches the named
        // file, leaving the tab placement and the Default file alone.
        for _ in 0..200 {
            cx.run_until_parked();
            if std::fs::read_to_string(&named)
                .is_ok_and(|s| layout::parse(&s).is_ok_and(|read| read == layout(cx)))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let saved = std::fs::read_to_string(&named).unwrap();
        assert_eq!(layout::parse(&saved).unwrap(), layout(cx));
        assert!(saved.contains("// review"));
        assert_eq!(layout(cx).tab_bar.place, TabsAt::Bottom);
        assert_eq!(std::fs::read_to_string(&common).unwrap(), original);
        open(cx, "item-branch");
        cx.dispatch_action(SwitchLayout {
            name: Some(layout::DEFAULT_NAME.into()),
        });
        wait_for(cx, "Default selected", &|cx| layout::active(cx).is_none());
        assert!(cx.read(|cx| ws.read(cx).bar_menu.is_none()));
        cx.dispatch_action(SwitchLayout {
            name: Some("Review".into()),
        });
        wait_for(cx, "Review restored", &|cx| {
            layout::active(cx).as_deref() == Some("Review")
        });
        assert_eq!(layout(cx), layout::parse(&saved).unwrap());
    }

    #[gpui::test]
    fn tabs_of_the_docks_are_moved_and_hidden_by_hand(cx: &mut TestAppContext) {
        use Panel::*;
        let root = db::testing::dir("ws-hand").canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "plain\n").unwrap();
        let config = db::testing::dir("ws-hand-config");
        let file = config.join("layout.json");
        std::fs::write(&file, "// mine\n{\n}\n").unwrap();
        let (ws, cx) = setup(cx, root.clone());
        cx.update(|_, cx| settings::reload_from(&config, cx));
        let none = gpui::Modifiers::default();
        // Where something is in the window as it is drawn now: what was
        // drawn once is remembered where it was.
        let at = |cx: &mut VisualTestContext, selector: &'static str| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("no {selector}"))
                .center()
        };
        let menu = |cx: &mut VisualTestContext, tab: &'static str, item: &'static str| {
            let tab = at(cx, tab);
            cx.simulate_mouse_down(tab, MouseButton::Right, none);
            cx.simulate_mouse_up(tab, MouseButton::Right, none);
            let item = at(cx, item);
            cx.simulate_click(item, none);
            cx.run_until_parked();
        };
        let drag = |cx: &mut VisualTestContext, tab: &'static str, to: &'static str| {
            let from = at(cx, tab);
            cx.simulate_mouse_down(from, MouseButton::Left, none);
            cx.simulate_mouse_move(from + gpui::point(px(6.), px(0.)), MouseButton::Left, none);
            // Where it goes may be drawn only now that a tab is held.
            let to = at(cx, to);
            cx.simulate_mouse_move(to, MouseButton::Left, none);
            cx.simulate_mouse_up(to, MouseButton::Left, none);
            cx.run_until_parked();
        };
        let layout = |cx: &mut VisualTestContext| cx.read(|cx| Layout::get(cx).clone());
        let docks = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.read(|cx| {
                let ws = ws.read(cx);
                (ws.left, ws.right, ws.bottom)
            })
        };
        // The file once all that was done has reached it.
        let written = |cx: &mut VisualTestContext| {
            for _ in 0..200 {
                cx.run_until_parked();
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                if layout::parse(&text).is_ok_and(|read| read == layout(cx)) {
                    return text;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("layout.json never said what the window shows");
        };

        // The menu of a tab hides its panel: Files, which was in front,
        // leaves the dock to the next panel it has.
        menu(cx, "panel-files", "menu-hide");
        assert_eq!(layout(cx).hidden, [Files]);
        assert_eq!(layout(cx).left.panels[0], Search);
        assert_eq!(docks(cx), (Some(Search), None, None));
        assert!(cx.read(|cx| ws.read(cx).dock_menu.is_none()));
        // Its command still opens it, and the menu of any tab of a dock
        // brings it back into that dock, where it is shown.
        menu(cx, "panel-git", "menu-show-files");
        assert!(layout(cx).hidden.is_empty());
        assert_eq!(layout(cx).left.panels.last(), Some(&Files));
        assert_eq!(docks(cx), (Some(Files), None, None));
        // To another dock, which opens on it.
        menu(cx, "panel-git", "menu-move-right");
        assert_eq!(layout(cx).right.panels, [Chat, Agent, Git]);
        assert_eq!(docks(cx), (Some(Files), Some(Git), None));
        let text = written(cx);
        assert!(text.contains("// mine"), "{text}");

        // A tab dragged onto another of its dock goes before it.
        drag(cx, "panel-files", "panel-database");
        assert_eq!(
            layout(cx).left.panels,
            [
                Search,
                Services,
                Files,
                Database,
                Api,
                Ai,
                Extensions,
                ExtensionViews
            ]
        );
        assert_eq!(docks(cx).0, Some(Files));
        // Onto a tab of another dock: before it there, and shown there.
        drag(cx, "panel-api", "panel-agent");
        assert_eq!(layout(cx).right.panels, [Chat, Api, Agent, Git]);
        assert_eq!(docks(cx), (Some(Files), Some(Api), None));
        // A closed dock has a place to drop a tab on while one is held,
        // and opens on what is dropped there.
        drag(cx, "panel-search", "drop-bottom");
        assert_eq!(layout(cx).bottom.panels.last(), Some(&Search));
        assert_eq!(docks(cx), (Some(Files), Some(Api), Some(Search)));
        assert!(cx.read(|cx| ws.read(cx).dragging.is_none()));
        // The tab in front dragged away: its dock shows what it has left.
        drag(cx, "panel-files", "panel-search");
        assert_eq!(docks(cx), (Some(Services), Some(Api), Some(Files)));
        written(cx);

        // Reset: the window as it comes, and the file put aside whole.
        cx.dispatch_action(ResetLayout);
        assert_eq!(docks(cx), (Some(Files), None, None));
        assert_eq!(layout(cx), Layout::default());
        for _ in 0..200 {
            cx.run_until_parked();
            if !file.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!file.exists());
        let old = std::fs::read_to_string(config.join("layout.json.old")).unwrap();
        assert!(
            old.contains("// mine") && old.contains("\"hidden\""),
            "{old}"
        );
        assert_eq!(layout(cx), Layout::default());
    }

    #[gpui::test]
    fn a_named_layout_is_saved_switched_and_remembered_for_open_projects(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let root = db::testing::dir("ws-named-layout").canonicalize().unwrap();
        let config = db::testing::dir("ws-named-layout-config");
        let common = config.join("layout.json");
        std::fs::write(
            &common,
            "// common\n{\"left\":{\"width\":310},\"open\":[\"files\"]}",
        )
        .unwrap();
        let (ws, cx) = setup(cx, root.clone());
        cx.update(|_, cx| settings::reload_from(&config, cx));
        let wait = |cx: &mut VisualTestContext, name: Option<&str>| {
            for _ in 0..200 {
                cx.executor().advance_clock(Duration::from_millis(50));
                cx.run_until_parked();
                let ready = cx.read(|cx| {
                    layout::active(cx).as_deref() == name
                        && std::fs::read_to_string(layout::path(cx)).is_ok_and(|text| {
                            layout::parse(&text).is_ok_and(|l| l == *Layout::get(cx))
                        })
                });
                if ready {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("layout never became {name:?}");
        };
        // A pending panel change is part of the saved snapshot. Saving
        // from the palette is a prompt; it never overwrites a named file.
        cx.dispatch_action(ShowSearch);
        cx.dispatch_action(SaveLayout);
        cx.simulate_input("Review");
        bounds_soon(cx, "layout-save");
        cx.simulate_keystrokes("enter");
        wait(cx, Some("Review"));
        assert!(cx.read(|cx| ws.read(cx).modal.is_none()));
        let review = config.join("layouts/Review.json");
        assert_eq!(cx.read(layout::path), review);
        assert_eq!(
            layout::parse(&std::fs::read_to_string(&review).unwrap())
                .unwrap()
                .open_in(Place::Left),
            Some(Panel::Search)
        );
        let common_text = std::fs::read_to_string(&common).unwrap();
        assert!(common_text.contains("// common"));

        // The selected file is the truth, including edits by hand and
        // Open Layout. The common layout is a separate one.
        cx.dispatch_action(ShowGit);
        wait(cx, Some("Review"));
        assert_eq!(cx.read(|cx| ws.read(cx).left), Some(Panel::Git));
        assert_eq!(std::fs::read_to_string(&common).unwrap(), common_text);
        cx.update(|window, cx| window.focus(&ws.focus_handle(cx)));
        cx.dispatch_action(OpenLayout);
        for _ in 0..200 {
            cx.run_until_parked();
            if active_path(&ws, cx) == Some(review.clone()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(active_path(&ws, cx), Some(review.clone()));
        std::fs::write(&review, "// mine\n{\"right\":{\"panels\":[\"files\"],\"width\":420},\"open\":[\"files\",\"git\"]}").unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| ws.read(cx).right), Some(Panel::Files));

        // Changing a panel, then selecting another layout immediately,
        // finishes the first file before the selection takes effect.
        cx.dispatch_action(ShowSearch);
        cx.dispatch_action(SwitchLayout {
            name: Some(layout::DEFAULT_NAME.into()),
        });
        wait(cx, None);
        assert_eq!(cx.read(|cx| ws.read(cx).left), Some(Panel::Search));
        let saved = layout::parse(&std::fs::read_to_string(&review).unwrap()).unwrap();
        assert_eq!(saved.open_in(Place::Left), Some(Panel::Search));
        assert_eq!(saved.open_in(Place::Right), Some(Panel::Files));
        assert!(
            std::fs::read_to_string(&review)
                .unwrap()
                .contains("// mine")
        );

        // The argument-bearing action can be bound to a key. With no
        // argument it opens the searchable list instead.
        std::fs::write(config.join("keymap.json"), r#"[{"context":"Workspace","bindings":{"alt-shift-r":["workspace::SwitchLayout",{"name":"Review"}]}}]"#).unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        cx.simulate_keystrokes("alt-shift-r");
        wait(cx, Some("Review"));
        cx.dispatch_action(SwitchLayout::default());
        cx.simulate_input("Default");
        bounds_soon(cx, "layout-choice-0");
        cx.simulate_keystrokes("enter");
        wait(cx, None);

        // Every open window follows a switch and remembers it for its
        // project. A window removed before another switch is left alone.
        let second_root = db::testing::dir("ws-named-second").canonicalize().unwrap();
        let second = cx.update(|_, cx| crate::open_workspace_window(second_root.clone(), None, cx));
        cx.dispatch_action(SwitchLayout {
            name: Some("Review".into()),
        });
        wait(cx, Some("Review"));
        let second_dock = second.read_with(cx, |w, _| w.right).unwrap();
        assert_eq!(second_dock, Some(Panel::Files));
        let choices: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(config.join("layouts.json")).unwrap())
                .unwrap();
        assert_eq!(choices["projects"][root.to_str().unwrap()], "Review");
        assert_eq!(choices["projects"][second_root.to_str().unwrap()], "Review");
        second
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
        cx.dispatch_action(SwitchLayout {
            name: Some(layout::DEFAULT_NAME.into()),
        });
        wait(cx, None);
        let reopened =
            cx.update(|_, cx| crate::open_workspace_window(second_root.clone(), None, cx));
        wait(cx, Some("Review"));
        assert_eq!(
            reopened.read_with(cx, |w, _| w.right).unwrap(),
            Some(Panel::Files)
        );
        reopened
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();

        // A broken target reports its own name and keeps the current one.
        std::fs::write(config.join("layouts/Broken.json"), "{bad").unwrap();
        cx.dispatch_action(SwitchLayout {
            name: Some("Broken".into()),
        });
        for _ in 0..200 {
            cx.run_until_parked();
            if cx.read(|cx| {
                cx.global::<settings::ConfigErrors>()
                    .0
                    .iter()
                    .any(|e| e.contains("Broken.json"))
            }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(cx.read(layout::active), Some("Review".into()));
        assert!(cx.read(|cx| {
            cx.global::<settings::ConfigErrors>()
                .0
                .iter()
                .any(|e| e.contains("Broken.json"))
        }));
        cx.dispatch_action(ResetLayout);
        wait(cx, Some("Review"));
        assert!(review.with_extension("json.old").exists());
        assert_eq!(cx.read(|cx| Layout::get(cx).clone()), Layout::default());
        // A layout without `open` restores its startup docks, including
        // when its sizes and panels match the currently selected layout.
        std::fs::write(&common, "{}").unwrap();
        cx.dispatch_action(ToggleChat);
        wait(cx, Some("Review"));
        cx.dispatch_action(SwitchLayout {
            name: Some(layout::DEFAULT_NAME.into()),
        });
        wait(cx, None);
        assert_eq!(
            cx.read(|cx| (ws.read(cx).left, ws.read(cx).right, ws.read(cx).bottom)),
            (Some(Panel::Files), None, None)
        );
    }

    #[gpui::test]
    fn the_window_starts_on_the_panels_it_was_left_on(cx: &mut TestAppContext) {
        let root = db::testing::dir("ws-open").canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "plain\n").unwrap();
        let config = db::testing::dir("ws-open-config");
        let file = config.join("layout.json");
        let (ws, cx) = setup(cx, root.clone());
        let docks = |cx: &mut VisualTestContext, ws: &Entity<Workspace>| {
            cx.run_until_parked();
            cx.read(|cx| {
                let ws = ws.read(cx);
                (ws.left, ws.right, ws.bottom)
            })
        };
        // The file once it says `open` is these panels.
        let kept = |cx: &mut VisualTestContext, open: &[Panel]| {
            for _ in 0..200 {
                cx.run_until_parked();
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                if layout::parse(&text).is_ok_and(|l| l.open.as_deref() == Some(open)) {
                    return text;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("layout.json never had {open:?} open");
        };
        // The user's own file, which says nothing of what is open: the
        // window is as it comes.
        std::fs::write(
            &file,
            "// mine\n{\n  \"left\": { \"width\": 300 } // narrow\n}\n",
        )
        .unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        assert_eq!(docks(cx, &ws), (Some(Panel::Files), None, None));

        // Search, and the chat right after: both are in the file, next to
        // what the user wrote there, and neither write lost the other's.
        cx.dispatch_action(ShowSearch);
        cx.dispatch_action(ToggleChat);
        let text = kept(cx, &[Panel::Search, Panel::Chat]);
        assert!(
            text.contains("// mine") && text.contains("// narrow"),
            "{text}"
        );
        assert_eq!(layout::parse(&text).unwrap().left.width, 300.);
        assert_eq!(
            docks(cx, &ws),
            (Some(Panel::Search), Some(Panel::Chat), None)
        );
        // A window opened now starts the same way.
        let start = |cx: &mut VisualTestContext| {
            let root = root.clone();
            cx.update(|window, cx| cx.new(|cx| Workspace::new(root, window, cx)))
        };
        let second = start(cx);
        assert_eq!(
            docks(cx, &second),
            (Some(Panel::Search), Some(Panel::Chat), None)
        );
        drop(second);

        // A dock closed by hand stays closed, and a terminal that was
        // open is not started again: its dock starts closed.
        cx.dispatch_action(ToggleSidebar);
        kept(cx, &[Panel::Chat]);
        ws.update_in(cx, |w, _, cx| {
            w.show_results = true;
            w.show_panel(Panel::Results, cx);
        });
        kept(cx, &[Panel::Chat, Panel::Results]);
        let third = start(cx);
        assert_eq!(docks(cx, &third), (None, Some(Panel::Chat), None));
        drop(third);

        // The file is the truth. Saved with other panels open, the
        // window follows; a panel with nothing to show does not open, and
        // the file is left as it was saved.
        let saved = r#"{ "open": ["agent", "terminal", "git"] }"#;
        std::fs::write(&file, saved).unwrap();
        cx.update(|_, cx| settings::reload_from(&config, cx));
        assert_eq!(docks(cx, &ws), (Some(Panel::Git), Some(Panel::Agent), None));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), saved);
        assert!(cx.read(|cx| cx.global::<settings::ConfigErrors>().0.is_empty()));
    }

    #[gpui::test]
    fn the_layout_file_sizes_the_window_and_a_mistake_keeps_the_last(cx: &mut TestAppContext) {
        let root = db::testing::dir("ws-layout").canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "plain\n").unwrap();
        let config = db::testing::dir("ws-layout-config");
        let (ws, cx) = setup(cx, root.clone());
        ws.update_in(cx, |w, _, cx| w.set_sidebar(Some(Panel::Files), cx));
        // Where a part was last drawn, once the window has drawn again.
        let size = |cx: &mut VisualTestContext, part: &'static str| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            let bounds = cx.debug_bounds(part).unwrap_or_else(|| panic!("no {part}"));
            (f32::from(bounds.size.width), f32::from(bounds.size.height))
        };
        let reload =
            |cx: &mut VisualTestContext| cx.update(|_, cx| settings::reload_from(&config, cx));
        let errors = |cx: &mut VisualTestContext| {
            cx.read(|cx| cx.global::<settings::ConfigErrors>().0.clone())
        };

        // With no file the parts have the sizes they always had.
        reload(cx);
        assert_eq!(size(cx, "dock-left").0, 390.);
        assert_eq!(size(cx, "status-bar").1, 26.);

        // The file names two parts; the rest stay.
        std::fs::write(
            config.join("layout.json"),
            "// narrower\n{ \"left\": { \"width\": 300 }, \"status_bar\": { \"height\": 32 } }\n",
        )
        .unwrap();
        reload(cx);
        assert!(errors(cx).is_empty(), "{:?}", errors(cx));
        assert_eq!(size(cx, "dock-left").0, 300.);
        assert_eq!(size(cx, "status-bar").1, 32.);
        assert_eq!(cx.read(|cx| Layout::get(cx).bottom.height), 280.);

        // A mistake in it is said, and the layout that was right stays.
        std::fs::write(
            config.join("layout.json"),
            "{ \"left\": { \"width\": \"wide\" } }",
        )
        .unwrap();
        reload(cx);
        let said = errors(cx);
        assert_eq!(said.len(), 1);
        assert!(said[0].starts_with("layout.json: "), "{said:?}");
        assert_eq!(size(cx, "dock-left").0, 300.);

        // Put right, the mistake is gone; taken away, so is the layout.
        std::fs::write(
            config.join("layout.json"),
            "{ \"left\": { \"width\": 2000 } }",
        )
        .unwrap();
        reload(cx);
        assert!(errors(cx).is_empty());
        assert_eq!(cx.read(|cx| Layout::get(cx).left.width), 900.);
        std::fs::remove_file(config.join("layout.json")).unwrap();
        reload(cx);
        assert_eq!(size(cx, "dock-left").0, 390.);
        assert_eq!(size(cx, "status-bar").1, 26.);

        // A border is dragged: the part follows the pointer, and when it
        // is let go its size is in the file, among what the user wrote.
        std::fs::write(
            config.join("layout.json"),
            "// mine\n{\n  \"tab_bar\": { \"height\": 30 } // lower\n}\n",
        )
        .unwrap();
        reload(cx);
        let none = gpui::Modifiers::default();
        let grip = |cx: &mut VisualTestContext, part: &'static str| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            cx.debug_bounds(part)
                .unwrap_or_else(|| panic!("no {part}"))
                .center()
        };
        let written = |cx: &mut VisualTestContext, what: &str| {
            for _ in 0..200 {
                cx.run_until_parked();
                let text = std::fs::read_to_string(config.join("layout.json")).unwrap_or_default();
                if text.contains(what) {
                    return text;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("layout.json never had {what}");
        };
        let at = grip(cx, "resize-left");
        cx.simulate_mouse_down(at, MouseButton::Left, none);
        cx.simulate_mouse_move(at + gpui::point(px(60.), px(5.)), MouseButton::Left, none);
        assert_eq!(cx.read(|cx| Layout::get(cx).left.width), 450.);
        assert_eq!(size(cx, "dock-left").0, 450.);
        // Nothing is written while it is held.
        assert!(
            !std::fs::read_to_string(config.join("layout.json"))
                .unwrap()
                .contains("\"left\"")
        );
        cx.simulate_mouse_up(at + gpui::point(px(60.), px(5.)), MouseButton::Left, none);
        let text = written(cx, "\"left\"");
        assert!(
            text.contains("// mine") && text.contains("// lower"),
            "{text}"
        );
        assert_eq!(layout::parse(&text).unwrap().left.width, 450.);
        assert_eq!(layout::parse(&text).unwrap().tab_bar.height, 30.);
        // Moving the pointer with nothing held sizes nothing.
        cx.simulate_mouse_move(at + gpui::point(px(200.), px(0.)), None, none);
        assert_eq!(cx.read(|cx| Layout::get(cx).left.width), 450.);

        // The chat's border is on its left: dragged left, the chat grows,
        // and no further than a window can show.
        ws.update(cx, |w, cx| {
            w.right = Some(Panel::Chat);
            cx.notify();
        });
        let at = grip(cx, "resize-right");
        cx.simulate_mouse_down(at, MouseButton::Left, none);
        cx.simulate_mouse_move(at - gpui::point(px(40.), px(0.)), MouseButton::Left, none);
        assert_eq!(cx.read(|cx| Layout::get(cx).right.width), 420.);
        cx.simulate_mouse_move(at + gpui::point(px(300.), px(0.)), MouseButton::Left, none);
        assert_eq!(cx.read(|cx| Layout::get(cx).right.width), 200.);
        cx.simulate_mouse_up(at, MouseButton::Left, none);
        written(cx, "\"right\"");

        // A double click on a border puts its part back, in the file too.
        let at = grip(cx, "resize-left");
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: none,
            click_count: 2,
            first_mouse: false,
        });
        assert_eq!(cx.read(|cx| Layout::get(cx).left.width), 390.);
        let text = written(cx, "390");
        assert_eq!(layout::parse(&text).unwrap().left.width, 390.);
        assert_eq!(layout::parse(&text).unwrap().right.width, 200.);
    }

    #[gpui::test]
    fn an_extension_brings_a_context_server_and_reads_its_settings(cx: &mut TestAppContext) {
        use crate::mcp_store::{McpStore, State};
        // Zed's real Postgres context server extension, installed. Its
        // code installs the server from npm and starts it with Node, which
        // here is a script that runs the stand-in server and hands it the
        // address the extension read from the user's settings.
        let root = db::testing::dir("ws-pg").canonicalize().unwrap();
        let data = db::testing::dir("ws-pg-data");
        let installed = data.join("extensions/zed/postgres-context-server");
        std::fs::create_dir_all(&installed).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../extension/tests/fixtures/postgres-context-server");
        for file in ["extension.toml", "extension.wasm"] {
            std::fs::copy(fixtures.join(file), installed.join(file)).unwrap();
        }
        let scratch = db::testing::dir("ws-pg-bin");
        let log = scratch.join("calls.jsonl");
        let node = scratch.join("node");
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ai/tests/fixtures/mock_mcp.py");
        executable(
            &node,
            &format!(
                "#!/bin/sh\nMOCK_MCP_LOG='{}' MOCK_MCP_KEY=\"$DATABASE_URL\" exec python3 '{}'\n",
                log.display(),
                mock.display()
            ),
        );
        cx.executor().allow_parking();
        let extensions = cx.update(|cx| {
            let store = cx.new(|cx| {
                let mut store =
                    ExtensionStore::new(data.join("extensions"), data.join("config"), cx);
                store.world = Some(std::sync::Arc::new(VueWorld {
                    node: node.to_string_lossy().into_owned(),
                    settings: store.settings_for(),
                }));
                store
            });
            ExtensionStore::set_global(store.clone(), cx);
            store.update(cx, |s, cx| s.scan(cx));
            store
        });
        let (_ws, cx) = setup(cx, root.clone());
        wait_for(cx, "the extensions folder", &|cx| {
            extensions.read(cx).loaded
        });
        assert_eq!(
            cx.read(|cx| extensions
                .read(cx)
                .find(Origin::Zed, "postgres-context-server")
                .unwrap()
                .provides()),
            "1 context server"
        );
        let mcp = cx.update(|_, cx| {
            let mcp = McpStore::global(cx);
            mcp.update(cx, |mcp, _| mcp.env = Some(std::env::vars().collect()));
            mcp
        });
        // Started as an agent task starts them, and waited for: each ends
        // up running or failed.
        let start = |cx: &mut VisualTestContext| {
            mcp.update(cx, |mcp, cx| mcp.start_all(&root, cx)).detach();
            wait_for(cx, "the context servers", &|cx| {
                !mcp.read(cx)
                    .servers
                    .iter()
                    .any(|entry| matches!(entry.state, State::Starting))
            });
        };
        let state = |cx: &mut VisualTestContext| {
            cx.read(|cx| {
                let mcp = mcp.read(cx);
                assert_eq!(mcp.servers.len(), 1);
                let entry = &mcp.servers[0];
                assert_eq!(entry.extension.as_deref(), Some("postgres-context-server"));
                match &entry.state {
                    State::Running(_, offer) => format!("{} tools", offer.tools.len()),
                    State::Failed(why) => why.to_string(),
                    _ => "not started".into(),
                }
            })
        };

        // The server is listed with no line in the settings. Started, the
        // extension says what it needs and does not have.
        start(cx);
        assert_eq!(state(cx), "missing `database_url` setting");
        // Its code installed the server on the way, which is written down.
        assert!(
            cx.read(|cx| extensions.read(cx).did("postgres-context-server"))
                .contains(&extension::Event::Installed(
                    "@zeddotdev/postgres-context-server".into()
                ))
        );

        // With the address in the settings, under the server's name, it
        // starts, and the server is told the address.
        let settings = serde_json::json!({ "context_servers": {
            "postgres-context-server": { "settings": { "database_url": "postgresql://localhost/app" } },
        } });
        cx.update(|_, cx| cx.set_global(settings::parse_settings(&settings.to_string()).unwrap()));
        cx.run_until_parked();
        start(cx);
        assert_eq!(state(cx), "4 tools");
        let tools = cx.read(|cx| mcp.read(cx).tools());
        let echo = tools
            .iter()
            .find(|tool| tool.spec.name == "mcp_postgres-context-server_echo")
            .expect("the server's tool under a name of its own");
        let said = echo
            .server
            .call(
                &echo.tool,
                serde_json::json!({ "text": "select 1" }),
                crate::mcp_store::CALL,
            )
            .unwrap();
        assert_eq!(said.0, "select 1\n[image]");
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.contains("postgresql://localhost/app"), "{logged}");

        // Turned off in the settings, or with its extension turned off, it
        // is stopped and gone from the list.
        let off = serde_json::json!({ "context_servers": {
            "postgres-context-server": { "enabled": false },
        } });
        cx.update(|_, cx| cx.set_global(settings::parse_settings(&off.to_string()).unwrap()));
        start(cx);
        assert!(cx.read(|cx| mcp.read(cx).servers.is_empty()));
        cx.update(|_, cx| cx.set_global(Settings::default()));
        extensions.update(cx, |s, cx| {
            s.set_off(Origin::Zed, "postgres-context-server", true, cx)
        });
        start(cx);
        assert!(cx.read(|cx| mcp.read(cx).servers.is_empty()));
    }
    #[gpui::test]
    fn native_extension_views_load_expand_run_refresh_and_clear_decorations(
        cx: &mut TestAppContext,
    ) {
        let Some(node) = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        }) else {
            eprintln!("skipped: no node to run an extension with");
            return;
        };
        let _languages = extension_languages();
        let (_config, store, ws, cx) =
            extension_setup(cx, "ext-native-views", "http://127.0.0.1:1");
        let installed = cx.read(|cx| store.read(cx).root.join("vscode/acme.views"));
        write_file(
            &installed.join("package.json"),
            r#"{
          "name":"views", "publisher":"Acme", "version":"1.0.0", "main":"main.js",
          "contributes":{"views":{"explorer":[{"id":"demo","name":"Demo tree"}]}}
        }"#,
        );
        write_file(
            &installed.join("main.js"),
            r#"
const v = require('vscode');
exports.activate = (context) => {
  const changes = new v.EventEmitter();
  const root = {label:'Root'}, child = {label:'Child'};
  const tree = v.window.createTreeView('demo', { treeDataProvider: {
    onDidChangeTreeData: changes.event,
    getChildren: (node) => node ? [child] : [root],
    getTreeItem: (node) => Object.assign(new v.TreeItem(node.label,node === root ? 1 : 0), {
      id: node === root ? 'root' : 'child', command:{command:'demo.choose',arguments:[node.label]},
    }),
  }});
  v.commands.registerCommand('demo.choose', (label) => { child.label = 'Updated'; changes.fire(); console.log('chosen',label); });
  const tests = v.tests.createTestController('demo','Tests');
  const test = tests.createTestItem('one','A long test name that leaves enough room for its complete action'); tests.items.add(test);
  tests.createRunProfile('Run', v.TestRunProfileKind.Run, (request) => {
    const run = tests.createTestRun(request); run.passed(test); run.end();
  }, true);
  const control = v.scm.createSourceControl('demo','Changes');
  control.createResourceGroup('modified','Modified').resourceStates = [
    {resourceUri:v.workspace.textDocuments[0].uri,command:{command:'demo.choose',arguments:['file']}},
  ];
  const kind = v.window.createTextEditorDecorationType({backgroundColor:new v.ThemeColor('editor.findMatchHighlightBackground'), after:{contentText:' annotation'}});
  const editor = v.window.activeTextEditor;
  editor.setDecorations(kind,[new v.Range(0,0,0,1)]);
  context.subscriptions.push(tree, tests, control, kind, v.window.registerFileDecorationProvider({provideFileDecoration: () => ({badge:'M',tooltip:'Changed by extension'})}));
};
"#,
        );
        store.update(cx, |store, cx| {
            store.world = Some(std::sync::Arc::new(NodeOnly(
                node.to_string_lossy().into_owned(),
            )));
            store.scan(cx);
        });
        wait_for(cx, "the manifest", &|cx| {
            store.read(cx).find(Origin::VsCode, "Acme.views").is_some()
        });
        store.update(cx, |store, cx| {
            store.allow(Origin::VsCode, "Acme.views", cx)
        });
        cx.dispatch_action(ShowExtensionViews);
        click(cx, "extension-view-0");
        let panel = cx.read(|cx| ws.read(cx).extension_views.clone());
        wait_for(cx, "the tree's root", &|cx| panel.read(cx).has("Root"));
        // The same native list lives in any dock its layout names.
        assert!(cx.read(|cx| ws.read(cx).shown(Panel::ExtensionViews)));
        let root = cx.read(|cx| panel.read(cx).index("Root").unwrap());
        panel.update(cx, |panel, cx| panel.pick(root, cx));
        wait_for(cx, "the child", &|cx| panel.read(cx).has("Child"));
        let child = cx.read(|cx| panel.read(cx).index("Child").unwrap());
        panel.update(cx, |panel, cx| panel.pick(child, cx));
        wait_for(cx, "the refreshed child", &|cx| {
            panel.read(cx).has("Updated")
        });
        assert!(cx.read(|cx| !panel.read(cx).has("Child")));
        let tests = cx.read(|cx| panel.read(cx).index("Tests").unwrap());
        cx.update(|_, cx| {
            let mut layout = Layout::get(cx).clone();
            layout.left.width = 280.;
            cx.set_global(layout);
        });
        ws.update(cx, |_, cx| cx.notify());
        panel.update(cx, |panel, cx| panel.pick(tests, cx));
        wait_for(cx, "the controller's tests", &|cx| {
            panel
                .read(cx)
                .has("A long test name that leaves enough room for its complete action")
        });
        let test = cx.read(|cx| {
            panel
                .read(cx)
                .index("A long test name that leaves enough room for its complete action")
                .unwrap()
        });
        // GPUI keeps debug selectors in a static callback even in a single test.
        let selector = Box::leak(format!("extension-view-action-{test}-0").into_boxed_str());
        let action = bounds_soon(cx, selector);
        let dock = bounds_soon(cx, "dock-left");
        assert!(action.left() >= dock.left());
        assert!(
            action.right() <= dock.right(),
            "{action:?} outside {dock:?}"
        );
        let all = cx.read(|cx| panel.read(cx).index("Run all").unwrap());
        panel.update(cx, |panel, cx| panel.pick(all, cx));
        wait_for(cx, "the test to pass", &|cx| {
            panel.read(cx).has_mark("Passed")
        });
        let editor = cx.read(|cx| ws.read(cx).active_editor().unwrap().clone());
        wait_for(cx, "the text annotation", &|cx| {
            !editor.read(cx).doc(cx).decorations().is_empty()
        });
        assert!(cx.read(|cx| {
            editor
                .read(cx)
                .doc(cx)
                .inlays()
                .iter()
                .any(|inlay| inlay.text == " annotation")
        }));
        let file = cx.read(|cx| editor.read(cx).doc(cx).path().unwrap().to_path_buf());
        store.update(cx, |store, cx| store.decorate_files(vec![file.clone()], cx));
        wait_for(cx, "the file badge", &|cx| {
            !store.read(cx).file_marks(&file).is_empty()
        });
        store.update(cx, |store, cx| {
            store.set_off(Origin::VsCode, "Acme.views", true, cx)
        });
        wait_for(cx, "its views and decorations to go", &|cx| {
            store.read(cx).views().is_empty() && editor.read(cx).doc(cx).decorations().is_empty()
        });
        assert!(cx.read(|cx| store.read(cx).file_marks(&file).is_empty()));
        assert!(cx.read(|cx| editor.read(cx).doc(cx).inlays().is_empty()));
    }
}
