//! The way into a project: the ones opened before, a folder to open, an
//! address to clone.
//!
//! It is one list, shown two ways. A start that has no window to put back
//! shows it in a window of its own, and a project opened from there takes
//! that window's place. Inside a project it is a list over the window, and
//! what is chosen gets a window next to this one.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

use gpui::{
    AnyElement, App, Bounds, Context, DismissEvent, Entity, FocusHandle, Focusable,
    PathPromptOptions, SharedString, Task, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions, div, point, prelude::*, px, size,
};

use crate::{
    fuzzy, git,
    picker::{Picker, PickerDelegate, highlighted_text},
    settings::Settings,
    theme::{ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
    windows,
};

#[derive(Clone, Debug, PartialEq)]
enum Row {
    /// Asks the system for a folder.
    Folder,
    /// Clones the address that was typed.
    Clone(String),
    /// A project opened before, with where the query matched its path.
    Recent(PathBuf, Vec<u32>),
}

/// A clone that is running. Let go of, it is stopped.
struct Cloning {
    into: PathBuf,
    said: String,
    stop: Arc<AtomicBool>,
    _task: Task<()>,
}

impl Drop for Cloning {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

pub struct WayIn {
    /// Whether this is the window a start shows: it closes when a project
    /// has opened, and Escape does not take it away.
    alone: bool,
    recent: Vec<PathBuf>,
    rows: Vec<Row>,
    selected: usize,
    cloning: Option<Cloning>,
    /// Why the last thing asked for was not done.
    failed: Option<String>,
}

impl WayIn {
    pub fn new(alone: bool, cx: &App) -> Self {
        Self {
            alone,
            recent: windows::recent(cx),
            rows: Vec::new(),
            selected: 0,
            cloning: None,
            failed: None,
        }
    }

    /// A path as the list writes it: from the home folder where it is
    /// under it.
    fn shown(path: &Path) -> String {
        match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf))
        {
            Some(rest) if rest.as_os_str().is_empty() => "~".into(),
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        }
    }

    /// Gives `root` a window and takes the list away: with its window if
    /// that is all the window was for.
    pub fn go(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let alone = self.alone;
        // A window is not opened in the middle of drawing or of a key
        // of another one.
        cx.spawn_in(window, async move |picker, cx| {
            cx.update(|_, cx| windows::open(root, cx)).ok();
            picker
                .update_in(cx, |_, window, cx| match alone {
                    true => window.remove_window(),
                    false => cx.emit(DismissEvent),
                })
                .ok();
        })
        .detach();
    }

    fn ask_folder(
        prompt: &'static str,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
        then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Picker<Self>>) + 'static,
    ) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |picker, cx| {
            if let Ok(Ok(Some(paths))) = answer.await
                && let Some(folder) = paths.into_iter().next()
            {
                picker
                    .update_in(cx, |picker, window, cx| {
                        then(&mut picker.delegate, folder, window, cx)
                    })
                    .ok();
            }
        })
        .detach();
    }

    /// Clones `address` into a folder of its name inside `parent`, and
    /// opens it when it is there.
    pub fn clone_into(
        &mut self,
        address: String,
        parent: PathBuf,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let Some(name) = git::clone_name(&address) else {
            self.failed = Some("This address names no repository".into());
            cx.notify();
            return;
        };
        let into = parent.join(name);
        let stop = Arc::new(AtomicBool::new(false));
        let (send, mut heard) = futures::channel::mpsc::unbounded::<String>();
        let clone = cx.background_executor().spawn({
            let (into, stop) = (into.clone(), stop.clone());
            async move {
                git::clone(&address, &into, &stop, |line| {
                    let _ = send.unbounded_send(line);
                })
            }
        });
        let task = cx.spawn_in(window, {
            let into = into.clone();
            async move |picker, cx| {
                use futures::StreamExt;
                // The sender goes with the clone: the lines end when it does.
                while let Some(line) = heard.next().await {
                    let known = picker.update(cx, |picker, cx| {
                        if let Some(cloning) = &mut picker.delegate.cloning {
                            cloning.said = line;
                            cx.notify();
                        }
                    });
                    if known.is_err() {
                        return;
                    }
                }
                let result = clone.await;
                picker
                    .update_in(cx, |picker, window, cx| {
                        // Not dropped from inside itself.
                        let done = picker.delegate.cloning.take();
                        match result {
                            Ok(()) => picker.delegate.go(into, window, cx),
                            Err(error) => picker.delegate.failed = Some(error.0),
                        }
                        cx.notify();
                        done
                    })
                    .ok();
            }
        });
        self.failed = None;
        self.cloning = Some(Cloning {
            into,
            said: "Starting".into(),
            stop,
            _task: task,
        });
        cx.notify();
    }

    #[cfg(test)]
    pub fn is_cloning(&self) -> bool {
        self.cloning.is_some()
    }

    #[cfg(test)]
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    #[cfg(test)]
    pub fn listed(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| match row {
                Row::Folder => "Open a folder".to_string(),
                Row::Clone(address) => format!("Clone {address}"),
                Row::Recent(path, _) => path.display().to_string(),
            })
            .collect()
    }
}

impl PickerDelegate for WayIn {
    fn placeholder(&self) -> SharedString {
        "Open a project, or paste an address to clone".into()
    }

    fn match_count(&self) -> usize {
        self.rows.len()
    }

    fn selected_index(&self) -> usize {
        self.selected
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Context<Picker<Self>>) {
        self.selected = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let query = query.trim();
        self.failed = None;
        self.rows = if git::is_address(query) {
            vec![Row::Clone(query.to_string())]
        } else if query.is_empty() {
            std::iter::once(Row::Folder)
                .chain(
                    self.recent
                        .iter()
                        .map(|path| Row::Recent(path.clone(), Vec::new())),
                )
                .collect()
        } else {
            let shown: Vec<String> = self.recent.iter().map(|path| Self::shown(path)).collect();
            fuzzy::fuzzy_match(shown.iter().map(String::as_str), query, 100, true)
                .into_iter()
                .map(|found| {
                    Row::Recent(
                        self.recent[found.index].clone(),
                        fuzzy::positions(&shown[found.index], query, true),
                    )
                })
                .chain(std::iter::once(Row::Folder))
                .collect()
        };
        self.selected = 0;
        Task::ready(())
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        // One clone at a time: what it says is on screen until it ends.
        if self.cloning.is_some() {
            return;
        }
        match self.rows.get(self.selected).cloned() {
            Some(Row::Folder) => Self::ask_folder("Open", window, cx, Self::go),
            Some(Row::Clone(address)) => {
                Self::ask_folder("Clone here", window, cx, move |this, parent, window, cx| {
                    this.clone_into(address, parent, window, cx)
                })
            }
            Some(Row::Recent(root, _)) if root.is_dir() => self.go(root, window, cx),
            Some(Row::Recent(root, _)) => {
                // Its folder is gone: it is no way in any more.
                windows::forget(&root, cx);
                self.recent.retain(|known| known != &root);
                self.rows
                    .retain(|row| !matches!(row, Row::Recent(known, _) if known == &root));
                self.selected = self.selected.min(self.rows.len().saturating_sub(1));
                self.failed = Some(format!("{} is not there any more", Self::shown(&root)));
                cx.notify();
            }
            None => {}
        }
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let theme = cx.theme();
        let row = div()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg);
        let aside = |text: String| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(UI_FONT_SMALL)
                .text_color(theme.fg_subtle)
                .child(text)
        };
        // What the first row says of the thing in progress or just failed.
        let state = (ix == 0)
            .then(|| match (&self.cloning, &self.failed) {
                (Some(cloning), _) => Some(format!(
                    "Cloning into {}: {}",
                    Self::shown(&cloning.into),
                    cloning.said
                )),
                (None, Some(failed)) => Some(failed.clone()),
                (None, None) => None,
            })
            .flatten();
        match &self.rows[ix] {
            Row::Folder => row
                .child("Open a folder")
                .children(state.map(aside))
                .into_any_element(),
            Row::Clone(address) => row
                .child(format!("Clone {address}"))
                .children(state.map(aside))
                .into_any_element(),
            Row::Recent(path, positions) => {
                let shown = Self::shown(path);
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| shown.clone());
                let open = windows::window_of(path, cx).is_some();
                row.child(div().flex_none().child(name))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(UI_FONT_SMALL)
                            .child(highlighted_text(
                                &shown,
                                positions,
                                theme.fg_subtle,
                                theme.accent,
                            )),
                    )
                    .when(open, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(UI_FONT_SMALL)
                                .text_color(theme.fg_subtle)
                                .child("open"),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    fn empty_text(&self) -> SharedString {
        "No project of that name".into()
    }

    fn width(&self) -> gpui::Pixels {
        px(640.)
    }
}

/// The window a start shows when it has no window to put back.
pub struct WayInWindow {
    list: Entity<Picker<WayIn>>,
    _subscriptions: [gpui::Subscription; 2],
}

impl WayInWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = cx.new(|cx| Picker::new(WayIn::new(true, cx), window, cx));
        window.focus(&list.focus_handle(cx));
        crate::theme::fit(window, cx);
        let follow = |_: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            let theme = Settings::get(cx).theme(window.appearance());
            crate::theme::put(theme, window, cx);
            window.refresh();
        };
        Self {
            list,
            _subscriptions: [
                cx.observe_window_appearance(window, follow),
                cx.observe_global_in::<Settings>(window, follow),
            ],
        }
    }

    #[cfg(test)]
    pub fn list(&self) -> &Entity<Picker<WayIn>> {
        &self.list
    }
}

impl Focusable for WayInWindow {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.list.focus_handle(cx)
    }
}

impl Render for WayInWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .gap_4()
            .pt(px(56.))
            .bg(theme.bg)
            .text_color(theme.fg)
            .font_family(Settings::get(cx).ui_font())
            .child(div().text_size(crate::theme::text(20.)).child("Solder"))
            .child(self.list.clone())
    }
}

/// Shows the way in as a window of its own.
pub fn open_window(cx: &mut App) -> WindowHandle<WayInWindow> {
    let bounds = Bounds::centered(None, size(px(760.), px(560.)), cx);
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
            crate::ensure_theme(window, cx);
            cx.new(|cx| WayInWindow::new(window, cx))
        },
    )
    .expect("failed to open a window")
}
