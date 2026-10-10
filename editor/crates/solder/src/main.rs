mod agent;
mod agent_panel;
mod agent_task;
mod ai_context;
mod ai_panel;
mod ai_providers;
mod ai_review;
mod ai_store;
mod api_panel;
mod bar_commands;
mod buffer_search;
mod chat_panel;
mod command_palette;
mod completion;
mod context_prompts;
mod database;
mod database_panel;
mod debug;
mod debug_launch;
mod debug_panel;
mod debug_timeline;
mod document;
mod editor;
mod editor_git;
mod editor_lsp;
mod element;
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
mod fuzzy;
mod git;
mod git_panel;
mod git_store;
mod go_to_line;
mod icons;
mod import_settings;
mod import_view;
mod indent;
mod inline_completion;
mod inline_edit;
mod ipynb;
mod key_layout;
mod key_layout_picker;
mod key_prompts;
mod layout;
mod layout_picker;
mod locations;
mod lsp_store;
mod mcp_store;
mod notebook;
mod perf;
mod picker;
mod plugin_store;
mod plugins_view;
mod project;
mod project_panel;
mod project_search;
mod reference_picker;
mod response;
mod results;
mod search;
mod services;
mod services_panel;
mod settings;
mod structure;
mod symbols;
mod terminal;
mod theme;
mod ui;
mod webview;
mod workspace;

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};

use crate::{perf::Perf, theme::Theme, workspace::Workspace};

// mimalloc: faster small allocations and less fragmentation than the system
// allocator, which shows up in both frame time and resident memory.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let started = Instant::now();
    // Set SOLDER_BENCH_STARTUP=1 to print time-to-first-frame and exit.
    let bench_startup = std::env::var_os("SOLDER_BENCH_STARTUP").is_some();

    let arg = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let arg = arg.canonicalize().unwrap_or(arg);
    let (root, file) = if arg.is_file() {
        (
            arg.parent().map(PathBuf::from).unwrap_or_default(),
            Some(arg),
        )
    } else {
        (arg, None)
    };
    // Read the file before the window exists, so the first frame is already
    // an editable file rather than an empty workspace.
    let initial = file.and_then(|path| {
        if path.extension().is_some_and(|ext| ext == "ipynb") {
            return Some((path, String::new()));
        }
        let bytes = std::fs::read(&path).ok()?;
        Some((path, String::from_utf8_lossy(&bytes).into_owned()))
    });

    Application::new()
        .with_assets(icons::Assets)
        .run(move |cx: &mut App| {
            cx.set_global(Perf::new(started));
            settings::reload(cx);
            settings::watch(cx);
            lsp_store::init(cx);

            let window = open_workspace_window(root, initial, cx);
            window
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
            if std::env::var_os("SOLDER_BENCH_TYPING").is_some() {
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

/// Opens a window on `root`, optionally with a file already loaded.
pub fn open_workspace_window(
    root: PathBuf,
    initial: Option<(PathBuf, String)>,
    cx: &mut App,
) -> WindowHandle<Workspace> {
    let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Solder".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(12.))),
            }),
            window_min_size: Some(size(px(480.), px(320.))),
            app_id: Some("dev.solder.Solder".into()),
            ..Default::default()
        },
        |window, cx| {
            if !cx.has_global::<Theme>() {
                let theme = settings::Settings::get(cx).theme(window.appearance());
                cx.set_global(theme);
            }
            let workspace = cx.new(|cx| Workspace::new(root, window, cx));
            if let Some((path, content)) = initial {
                workspace.update(cx, |w, cx| {
                    if path.extension().is_some_and(|ext| ext == "ipynb") {
                        w.open_path(path, None, window, cx);
                    } else {
                        w.add_editor(Some(path), &content, None, window, cx);
                    }
                });
            }
            workspace.update(cx, |w, cx| w.start_session(window, cx));
            workspace
        },
    )
    .expect("failed to open a window")
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
