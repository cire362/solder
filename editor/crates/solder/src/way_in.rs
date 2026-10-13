//! The way into a project: the ones opened before, a folder to open, an
//! address to clone, or an SSH project.
//!
//! It is one list, shown two ways. A start that has no window to put back
//! shows it in a window of its own, and a project opened from there takes
//! that window's place. Inside a project it is a list over the window, and
//! what is chosen gets a window next to this one.

use crate::lang::{t, tf};
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
    remote::{Endpoint, Target},
    remote_ui,
    settings::Settings,
    theme::{ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
    windows,
    workspace::remote_workspace,
};

#[derive(Clone, Debug, PartialEq)]
enum Row {
    /// Asks the system for a folder.
    Folder,
    Ssh,
    Connect(String),
    Back,
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
    ssh: bool,
    connecting: Option<(remote_workspace::execution::Cancellation, Task<()>)>,
    _escape: Option<gpui::Subscription>,
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
            ssh: false,
            connecting: None,
            _escape: None,
            failed: None,
        }
    }

    fn ssh_mode(&mut self, ssh: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.connecting = None;
        self.ssh = ssh;
        self.update_matches(String::new(), window, cx).detach();
        cx.defer_in(window, |picker, _, cx| picker.set_query("", cx));
        cx.notify();
    }

    fn connect(&mut self, target: Target, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let cancellation = remote_workspace::execution::Cancellation::new();
        let cancel = cancellation.0.clone();
        self.failed = None;
        let task = cx.spawn_in(window, async move |picker, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { remote_workspace::connect_cached(target) })
                .await;
            if cancel.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            let result = match result {
                Ok(mount) => cx
                    .update(|_, cx| {
                        // Keep the registration alive until the destination owns it.
                        windows::open(mount.root.clone(), cx).update(cx, |workspace, window, cx| {
                            workspace.retain_remote(mount, cx);
                            workspace.resume_remote_session(window, cx);
                        })
                    })
                    .and_then(|result| result)
                    .map_err(std::io::Error::other),
                Err(error) => Err(error),
            };
            picker
                .update_in(cx, |picker, window, cx| {
                    let done = picker.delegate.connecting.take();
                    match result {
                        Ok(()) if picker.delegate.alone => window.remove_window(),
                        Ok(()) => cx.emit(DismissEvent),
                        Err(error) => {
                            picker.delegate.failed =
                                Some(tf("SSH connection failed: {}", &[&error]))
                        }
                    }
                    cx.notify();
                    done
                })
                .ok();
        });
        self.connecting = Some((cancellation, task));
        cx.notify();
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
            cx.update(|_, cx| windows::open_project(root, cx)).ok();
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
            self.failed = Some(t("This address names no repository").into());
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
            said: t("Starting").into(),
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
                Row::Folder => t("Open a folder").to_string(),
                Row::Ssh => t("Connect to an SSH project").to_string(),
                Row::Connect(input) => input.clone(),
                Row::Back => t("Back").to_string(),
                Row::Clone(address) => tf("Clone {}", &[&address]),
                Row::Recent(path, _) => path.display().to_string(),
            })
            .collect()
    }
}

impl PickerDelegate for WayIn {
    fn placeholder(&self) -> SharedString {
        if self.ssh {
            t("SSH connection: ssh://user@host:port/path or alias:/path").into()
        } else {
            t("Open a project, or paste an address to clone").into()
        }
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
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        if self._escape.is_none() {
            let picker = cx.weak_entity();
            self._escape = Some(cx.intercept_keystrokes(move |event, window, cx| {
                if event.keystroke.key == "escape" {
                    picker
                        .update(cx, |picker, cx| {
                            if picker.focus_handle(cx).is_focused(window)
                                && (picker.delegate.ssh || picker.delegate.alone)
                            {
                                cx.stop_propagation();
                                if picker.delegate.ssh {
                                    picker.delegate.ssh_mode(false, window, cx);
                                }
                            }
                        })
                        .ok();
                }
            }));
        }
        let query = if self.ssh {
            query.as_str()
        } else {
            query.trim()
        };
        self.failed = None;
        self.rows = if self.ssh {
            vec![Row::Connect(query.to_string()), Row::Back]
        } else if git::is_address(query) {
            vec![Row::Clone(query.to_string())]
        } else if query.is_empty() {
            [Row::Folder, Row::Ssh]
                .into_iter()
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
                .chain([Row::Ssh, Row::Folder])
                .collect()
        };
        self.selected = 0;
        Task::ready(())
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if matches!(self.rows.get(self.selected), Some(Row::Back)) {
            self.ssh_mode(false, window, cx);
            return;
        }
        // Keep each operation's progress visible until it ends or is cancelled.
        if self.cloning.is_some() || self.connecting.is_some() {
            return;
        }
        match self.rows.get(self.selected).cloned() {
            Some(Row::Folder) => Self::ask_folder(t("Open"), window, cx, Self::go),
            Some(Row::Ssh) => self.ssh_mode(true, window, cx),
            Some(Row::Connect(input)) => match remote_ui::parse_target(&input) {
                Ok(target) => self.connect(target, window, cx),
                Err(_) => {
                    self.failed = Some(t("Use ssh://user@host:port/path or alias:/path").into());
                    cx.notify();
                }
            },
            Some(Row::Back) => unreachable!("handled before the busy check"),
            Some(Row::Clone(address)) => Self::ask_folder(
                t("Clone here"),
                window,
                cx,
                move |this, parent, window, cx| this.clone_into(address, parent, window, cx),
            ),
            Some(Row::Recent(root, _))
                if remote_workspace::own_cache_root(&root) || root.is_dir() =>
            {
                self.go(root, window, cx)
            }
            Some(Row::Recent(root, _)) => {
                // Its folder is gone: it is no way in any more.
                windows::forget(&root, cx);
                self.recent.retain(|known| known != &root);
                self.rows
                    .retain(|row| !matches!(row, Row::Recent(known, _) if known == &root));
                self.selected = self.selected.min(self.rows.len().saturating_sub(1));
                self.failed = Some(tf("{} is not there any more", &[&Self::shown(&root)]));
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
            .then(|| match (&self.connecting, &self.cloning, &self.failed) {
                (Some(_), _, _) => Some(t("Connecting to SSH").into()),
                (_, Some(cloning), _) => Some(tf(
                    "Cloning into {}: {}",
                    &[&Self::shown(&cloning.into), &cloning.said],
                )),
                (_, None, Some(failed)) => Some(failed.clone()),
                _ => None,
            })
            .flatten();
        match &self.rows[ix] {
            Row::Folder => row
                .child(t("Open a folder"))
                .children(state.map(aside))
                .into_any_element(),
            Row::Ssh => row.child(t("Connect to an SSH project")).into_any_element(),
            Row::Connect(input) => {
                let label = match remote_ui::parse_target(input) {
                    Ok(Target {
                        endpoint: Endpoint::Ssh { host, port },
                        path,
                    }) => tf("Connect to {}:{}: {}", &[&host, &port, &path]),
                    _ => t("Enter an SSH connection and an absolute project path").into(),
                };
                row.child(label)
                    .children(state.map(aside))
                    .into_any_element()
            }
            Row::Back => row
                .child(if self.connecting.is_some() {
                    t("Cancel")
                } else {
                    t("Back")
                })
                .into_any_element(),
            Row::Clone(address) => row
                .child(tf("Clone {}", &[&address]))
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
                                .child(t("open")),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    fn empty_text(&self) -> SharedString {
        t("No project of that name").into()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn ssh_entry_validates_without_connecting_and_escape_goes_back(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(crate::perf::Perf::new(std::time::Instant::now()));
            cx.set_global(Settings::default());
            crate::picker::bind_keys(cx);
        });
        for alone in [true, false] {
            let (picker, cx) = cx.add_window_view(|window, cx| {
                crate::theme::put(crate::theme::Theme::dark(), window, cx);
                Picker::new(WayIn::new(alone, cx), window, cx)
            });
            let dismissals = std::rc::Rc::new(std::cell::Cell::new(0));
            let received = dismissals.clone();
            let _subscription = cx.update(|_, cx| {
                cx.subscribe(&picker, move |_, _: &DismissEvent, _| {
                    received.set(received.get() + 1);
                })
            });
            picker.update_in(cx, |picker, window, cx| {
                assert_eq!(
                    picker.delegate.listed(),
                    ["Open a folder", "Connect to an SSH project"]
                );
                window.focus(&picker.focus_handle(cx));
            });
            cx.simulate_keystrokes("down enter");
            picker.update_in(cx, |picker, window, cx| {
                assert!(picker.delegate.ssh);
                picker.set_query("ssh://user@host:2222/srv/app", cx);
                picker.refresh(window, cx);
                assert!(matches!(&picker.delegate.rows[0], Row::Connect(input) if remote_ui::parse_target(input).is_ok()));
                assert!(picker.delegate.connecting.is_none());
                assert!(picker.delegate.cloning.is_none());
            });
            for input in [
                "host:relative",
                "ssh://user:password@host/path",
                "ssh://host/%0A",
            ] {
                picker.update_in(cx, |picker, window, cx| {
                    picker.set_query(input, cx);
                    picker.refresh(window, cx);
                    picker.delegate.confirm(window, cx);
                    assert!(picker.delegate.failure().is_some());
                    assert!(picker.delegate.connecting.is_none());
                    assert!(picker.delegate.ssh);
                });
            }
            let cancellation = remote_workspace::execution::Cancellation::new();
            let cancelled = cancellation.0.clone();
            picker.update_in(cx, |picker, window, cx| {
                picker.delegate.connecting = Some((cancellation, Task::ready(())));
                if alone {
                    picker.delegate.selected = 1;
                    picker.delegate.confirm(window, cx);
                }
            });
            if !alone {
                cx.simulate_keystrokes("escape");
            }
            assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
            assert!(cx.read(|cx| !picker.read(cx).delegate.ssh));
            assert_eq!(dismissals.get(), 0);
            cx.simulate_keystrokes("escape");
            assert_eq!(dismissals.get(), usize::from(!alone));
        }
    }
}
