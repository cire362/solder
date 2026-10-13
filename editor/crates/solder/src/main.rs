mod agent;
mod agent_panel;
mod agent_task;
mod ai_commit;
mod ai_context;
mod ai_panel;
mod ai_providers;
mod ai_review;
mod ai_store;
mod api_panel;
mod bar_commands;
mod breakpoint;
mod buffer_search;
mod chat_panel;
mod command_palette;
mod completion;
mod container_ui;
mod context_prompts;
mod database;
mod database_panel;
mod debug;
mod debug_launch;
mod debug_panel;
mod debug_timeline;
mod devcontainer;
mod document;
mod editor;
mod editor_git;
mod editor_lsp;
mod element;
mod encoding_picker;
mod erd_view;
mod extension_api;
mod extension_ask;
mod extension_decorations;
mod extension_store;
mod extension_views;
mod extensions_panel;
mod file_diff;
mod file_finder;
mod file_icons;
mod file_save;
mod fuzzy;
mod git;
mod git_history;
mod git_history_panel;
mod git_panel;
mod git_pr;
mod git_pr_panel;
mod git_secrets;
mod git_stash;
mod git_store;
mod go_to_line;
mod icons;
mod import_settings;
mod import_view;
mod indent;
mod inline_completion;
mod inline_edit;
mod ipynb;
mod journal;
mod key_layout;
mod key_layout_picker;
mod key_prompts;
mod lang;
mod layout;
mod layout_picker;
mod locations;
mod lsp_store;
mod markdown;
mod mcp_store;
mod notebook;
mod outline;
#[cfg(test)]
mod patience;
mod perf;
mod picker;
mod plugin_store;
mod plugins_view;
mod problems_panel;
mod project;
mod project_panel;
mod project_search;
mod python_debug;
mod reference_picker;
mod remote;
mod remote_ui;
mod response;
mod results;
mod search;
mod services;
mod services_panel;
mod settings;
mod structure;
mod symbols;
mod terminal;
mod terminal_output;
mod test_runner;
mod tests_panel;
mod theme;
mod ui;
mod viewer;
mod way_in;
mod webview;
mod windows;
mod workspace;

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui::{
    App, AppContext, Application, TitlebarOptions, Window, WindowHandle, WindowOptions, point, px,
    size,
};

use crate::{perf::Perf, theme::Theme, workspace::Workspace};

// mimalloc: faster small allocations and less fragmentation than the system
// allocator, which shows up in both frame time and resident memory.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let started = Instant::now();
    journal::start(journal::dir());
    // Set SOLDER_BENCH_STARTUP=1 to print time-to-first-frame and exit.
    let bench_startup = std::env::var_os("SOLDER_BENCH_STARTUP").is_some();

    let benchmark =
        std::env::vars_os().any(|(name, _)| name.to_string_lossy().starts_with("SOLDER_BENCH"));
    // A folder or a file named on the command line is what is opened.
    // With none named, the windows of the last run come back.
    let named = std::env::args_os().nth(1).map(|arg| {
        let arg = PathBuf::from(arg);
        let arg = arg.canonicalize().unwrap_or(arg);
        let (root, file) = if arg.is_file() {
            (
                arg.parent().map(PathBuf::from).unwrap_or_default(),
                Some(arg),
            )
        } else {
            (arg, None)
        };
        // Read the file before the window exists, so the first frame is
        // already an editable file rather than an empty workspace.
        let cached_root = remote::cache_root(&root);
        let cached = cached_root.is_some();
        let root = cached_root.unwrap_or(root);
        let remote_file = cached.then(|| file.clone()).flatten();
        if cached {
            remote::mark_closed(root.clone());
        }
        let initial = file.filter(|_| !cached).and_then(|path| {
            // A notebook and a picture are not read as text.
            if !opens_as_text(&path) {
                let nothing = document::Loaded::of(&[]);
                return Some((path, nothing));
            }
            let loaded = document::load(&path).ok()?;
            Some((path, loaded))
        });
        (root, initial, remote_file)
    });
    // Nothing is kept of a benchmark's windows, and it is shown none of
    // an earlier run's.
    let kept_in = (!benchmark).then(windows::path);
    // Which windows to make is known before the first one, like the file.
    let kept = match (&named, &kept_in) {
        (None, Some(path)) => Some(windows::Kept::read(path)),
        _ => None,
    };

    Application::new()
        .with_assets(icons::Assets)
        .run(move |cx: &mut App| {
            cx.set_global(Perf::new(started));
            settings::reload(cx);
            settings::watch(cx);
            lsp_store::init(cx);
            windows::init(kept_in.clone(), cx);

            let window = match named {
                Some((root, initial, remote_file)) => {
                    // The projects opened before are not needed to show
                    // this one: they are read while it is drawn.
                    windows::learn_from(kept_in, cx);
                    if workspace::remote_workspace::own_cache_root(&root) {
                        windows::open_cached_file(windows::Place::of(root), remote_file, cx);
                        None
                    } else {
                        Some(open_workspace_window(windows::Place::of(root), initial, cx))
                    }
                }
                None => {
                    let kept = kept.unwrap_or_default();
                    windows::learn(&kept, cx);
                    if windows::restore(&kept, cx) == 0 {
                        way_in::open_window(cx);
                    }
                    None
                }
            };
            if let Some(first) = cx.windows().first() {
                first
                    .update(cx, |_, window, _| {
                        window.on_next_frame(move |_, cx| {
                            let perf = cx.global_mut::<Perf>();
                            let elapsed = perf.process_start.elapsed();
                            perf.first_frame = Some(elapsed);
                            if bench_startup {
                                println!("first_frame_ms={:.1}", elapsed.as_secs_f64() * 1000.0);
                                cx.quit();
                            }
                        });
                    })
                    .ok();
            }
            if let Some(window) = window
                && std::env::var_os("SOLDER_BENCH_TYPING").is_some()
            {
                bench_typing(window, cx);
            }

            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

/// Whether a file named on the command line is read here, before the
/// window, to be its first tab of text.
fn opens_as_text(path: &std::path::Path) -> bool {
    !(path.extension().is_some_and(|ext| ext == "ipynb") || viewer::is_picture(path))
}

/// The theme of the settings, for the first window that asks.
pub fn ensure_theme(window: &Window, cx: &mut App) {
    if !cx.has_global::<Theme>() {
        let theme = settings::Settings::get(cx).theme(window.appearance());
        cx.set_global(theme);
    }
}

/// Opens a window on a project where `place` says, optionally with a
/// file already loaded.
pub fn open_workspace_window(
    place: windows::Place,
    initial: Option<(PathBuf, document::Loaded)>,
    cx: &mut App,
) -> WindowHandle<Workspace> {
    let (window_bounds, display_id) = place.options(size(px(1280.), px(820.)), cx);
    let root = place.root;
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                display_id,
                titlebar: Some(TitlebarOptions {
                    title: Some("Solder".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(14.), px(12.))),
                }),
                window_min_size: Some(size(px(480.), px(320.))),
                app_id: Some("dev.solder.Solder".into()),
                ..Default::default()
            },
            {
                let root = root.clone();
                move |window, cx| {
                    ensure_theme(window, cx);
                    let unavailable =
                        remote::is_remote(&root) && remote::mount(&root).ok().flatten().is_none();
                    let workspace = cx.new(|cx| Workspace::new(root, window, cx));
                    if let Some((path, loaded)) = initial {
                        workspace.update(cx, |w, cx| {
                            if !opens_as_text(&path) {
                                w.open_path(path, None, window, cx);
                            } else {
                                w.add_loaded(path, &loaded, None, window, cx);
                            }
                        });
                    }
                    if !unavailable {
                        workspace.update(cx, |w, cx| w.start_session(window, cx));
                    }
                    workspace
                }
            },
        )
        .expect("failed to open a window");
    windows::opened(&root, window, cx);
    window
}

/// Types 300 characters into the open file, one per 16 ms, then prints input
/// latency and frame time percentiles and exits. Run with
/// `SOLDER_BENCH_TYPING=1 solder <file>`.
fn bench_typing(window: WindowHandle<Workspace>, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(500))
            .await;
        for i in 0..300 {
            let text = if i % 40 == 39 { "\n" } else { "x" };
            window
                .update(cx, |workspace, window, cx| {
                    workspace.bench_insert(text, window, cx)
                })
                .ok();
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
        }
        cx.update(|cx| {
            let perf = cx.global::<Perf>();
            let (i50, i99) = perf.input_p50_p99().unwrap_or_default();
            let (f50, f99) = perf.frame_p50_p99().unwrap_or_default();
            println!(
                "input_p50_ms={} input_p99_ms={} frame_p50_ms={} frame_p99_ms={} memory_mb={}",
                perf::ms(i50),
                perf::ms(i99),
                perf::ms(f50),
                perf::ms(f99),
                perf::resident_memory().unwrap_or(0) / (1024 * 1024)
            );
            cx.quit();
        })
        .ok();
    })
    .detach();
}
