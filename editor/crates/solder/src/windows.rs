//! The windows that are open and the projects opened before.
//!
//! A project has one window. Which projects have a window, where each
//! window is and which projects had one earlier is kept in `windows.json`
//! next to the tabs, so that a start with no folder named puts back the
//! windows the last run was left with. The window that closes last is the
//! one that comes back: closing it ends the run, it is not a choice to
//! have no window.

use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use gpui::{
    App, Bounds, DisplayId, Global, Pixels, Task, Window, WindowBounds, WindowHandle, WindowId,
    point, px, size,
};
use serde::{Deserialize, Serialize};

use crate::workspace::Workspace;

/// How many projects opened before are remembered.
const RECENT: usize = 30;
/// More windows than this is not a set anybody left: the file is not ours.
const OPEN: usize = 32;
/// A window dragged or sized says so many times a second: the file is
/// written when it has stopped.
const SETTLE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

/// A project's window: where it is on which display.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub root: PathBuf,
    /// Left, top, width and height, from the corner of its display.
    #[serde(default)]
    pub bounds: Option<[f32; 4]>,
    #[serde(default)]
    pub shape: Shape,
    #[serde(default)]
    pub display: Option<String>,
}

impl Place {
    pub fn of(root: PathBuf) -> Self {
        Self {
            root,
            ..Self::default()
        }
    }

    fn at(root: PathBuf, window: &Window, cx: &App) -> Self {
        let (shape, bounds) = match window.window_bounds() {
            WindowBounds::Windowed(bounds) => (Shape::Windowed, bounds),
            WindowBounds::Maximized(bounds) => (Shape::Maximized, bounds),
            WindowBounds::Fullscreen(bounds) => (Shape::Fullscreen, bounds),
        };
        Self {
            root,
            bounds: Some([
                bounds.origin.x.into(),
                bounds.origin.y.into(),
                bounds.size.width.into(),
                bounds.size.height.into(),
            ]),
            shape,
            display: window
                .display(cx)
                .and_then(|display| display.uuid().ok())
                .map(|uuid| uuid.to_string()),
        }
    }

    /// Where a window for this goes now: on the display it was on if that
    /// is still there, and inside it. A display that went, or a place that
    /// is off what is left, gives the middle of the main one.
    pub fn options(
        &self,
        fallback: gpui::Size<Pixels>,
        cx: &App,
    ) -> (WindowBounds, Option<DisplayId>) {
        let display = self.display.as_ref().and_then(|wanted| {
            cx.displays()
                .into_iter()
                .find(|display| display.uuid().is_ok_and(|uuid| &uuid.to_string() == wanted))
        });
        let kept = self.bounds.filter(|b| b.iter().all(|n| n.is_finite()));
        let bounds = match (&display, kept) {
            (Some(display), Some([x, y, width, height]))
                if fits(display.bounds().size, [x, y, width, height]) =>
            {
                Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
            }
            _ => {
                let wanted = kept.map_or(fallback, |[_, _, width, height]| {
                    size(px(width.max(480.)), px(height.max(320.)))
                });
                return (self.shaped(Bounds::centered(None, wanted, cx)), None);
            }
        };
        (self.shaped(bounds), display.map(|display| display.id()))
    }

    fn shaped(&self, bounds: Bounds<Pixels>) -> WindowBounds {
        match self.shape {
            Shape::Windowed => WindowBounds::Windowed(bounds),
            Shape::Maximized => WindowBounds::Maximized(bounds),
            Shape::Fullscreen => WindowBounds::Fullscreen(bounds),
        }
    }
}

/// Whether enough of a window is on a display of this size to take hold
/// of it: its title bar's left part, at least.
fn fits(display: gpui::Size<Pixels>, [x, y, width, height]: [f32; 4]) -> bool {
    let (wide, tall): (f32, f32) = (display.width.into(), display.height.into());
    width >= 200.
        && height >= 120.
        && x + width >= 120.
        && x <= wide - 120.
        && y >= 0.
        && y <= tall - 60.
}

/// What the file holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Kept {
    pub version: u32,
    pub open: Vec<Place>,
    /// The projects opened before, the last one first.
    pub recent: Vec<PathBuf>,
}

impl Kept {
    /// What an earlier run left, or nothing if the file is not there or
    /// is not what this writes.
    pub fn read(path: &Path) -> Self {
        let read = || {
            if std::fs::metadata(path).ok()?.len() > 1024 * 1024 {
                return None;
            }
            let kept: Self = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            (kept.version == 1 && kept.open.len() <= OPEN && kept.recent.len() <= RECENT * 4)
                .then_some(kept)
        };
        read().unwrap_or_default()
    }
}

/// Puts `root` first among the projects opened before.
fn remember(recent: &mut Vec<PathBuf>, root: &Path) {
    recent.retain(|known| known != root);
    recent.insert(0, root.to_path_buf());
    recent.truncate(RECENT);
}

type Sent = (Kept, Option<futures::channel::oneshot::Sender<()>>);

struct Windows {
    /// Nothing is kept without a file to keep it in: in tests that name
    /// none, and under a benchmark.
    writer: Option<mpsc::Sender<Sent>>,
    open: Vec<(WindowId, Place)>,
    recent: Vec<PathBuf>,
    /// Windows close one after another when the run ends. That is not
    /// the user closing them.
    quitting: bool,
    /// Whether what an earlier run kept has been read. Until then
    /// nothing is written: it would be written over what was not read.
    learned: bool,
    settle: Option<Task<()>>,
}

impl Global for Windows {}

impl Windows {
    fn kept(&self) -> Kept {
        Kept {
            version: 1,
            open: self.open.iter().map(|(_, place)| place.clone()).collect(),
            recent: self.recent.clone(),
        }
    }
}

pub fn path() -> PathBuf {
    crate::settings::config_dir()
        .join("sessions")
        .join("windows.json")
}

/// Starts remembering windows. `path` is the file to keep them in, if
/// they are kept at all.
pub fn init(path: Option<PathBuf>, cx: &mut App) {
    let writer = path.map(|path| {
        let (send, receive) = mpsc::channel::<Sent>();
        // One thread, so that a later set never lands before an earlier.
        std::thread::spawn(move || {
            for (kept, done) in receive {
                let result = serde_json::to_vec_pretty(&kept)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| crate::ipynb::write_atomic(&path, &bytes));
                if let Err(error) = result {
                    crate::journal::error(format_args!("could not keep the windows: {error}"));
                }
                if let Some(done) = done {
                    let _ = done.send(());
                }
            }
        });
        send
    });
    cx.set_global(Windows {
        writer,
        open: Vec::new(),
        recent: Vec::new(),
        quitting: false,
        learned: false,
        settle: None,
    });
    cx.on_app_quit(|cx| {
        let done = cx.try_global::<Windows>().and_then(|windows| {
            let (send, receive) = futures::channel::oneshot::channel();
            let writer = windows.writer.as_ref().filter(|_| windows.learned)?;
            writer.send((windows.kept(), Some(send))).ok()?;
            Some(receive)
        });
        quitting(cx);
        async move {
            if let Some(done) = done {
                done.await.ok();
            }
        }
    })
    .detach();
}

/// Wait for the current window set before the platform's short quit timeout.
pub(crate) fn flush(cx: &App) -> Task<()> {
    let done = (|| {
        let windows = cx.try_global::<Windows>()?;
        let writer = windows.writer.as_ref().filter(|_| windows.learned)?;
        let (send, receive) = futures::channel::oneshot::channel();
        writer.send((windows.kept(), Some(send))).ok()?;
        Some(receive)
    })();
    cx.background_executor().spawn(async move {
        if let Some(done) = done {
            done.await.ok();
        }
    })
}

/// The user closed a window, and it is out of the set: unless it is the
/// only one, which is what the next start comes back to. A window that
/// goes because the run ends was not closed by anybody, so this is said
/// by the window when it agrees to close, not heard from the system.
pub fn closed(id: WindowId, cx: &mut App) {
    if !cx.has_global::<Windows>() {
        return;
    }
    let windows = cx.global_mut::<Windows>();
    let known = windows.open.iter().any(|(open, _)| *open == id);
    if windows.quitting || !known || windows.open.len() == 1 {
        return;
    }
    windows.open.retain(|(open, _)| *open != id);
    changed(cx);
}

/// The run is ending: windows that close from here on stay in the set.
pub fn quitting(cx: &mut App) {
    if cx.has_global::<Windows>() {
        let windows = cx.global_mut::<Windows>();
        windows.quitting = true;
        windows.settle = None;
    }
}

/// Takes in the projects an earlier run opened: after the ones this run
/// has opened already, which are the later ones.
pub fn learn(kept: &Kept, cx: &mut App) {
    let windows = cx.global_mut::<Windows>();
    for root in &kept.recent {
        if !windows.recent.contains(root) && windows.recent.len() < RECENT {
            windows.recent.push(root.clone());
        }
    }
    windows.learned = true;
    changed(cx);
}

/// Reads what an earlier run kept, off the thread that draws, and takes
/// it in.
pub fn learn_from(path: Option<PathBuf>, cx: &mut App) {
    let Some(path) = path else {
        return learn(&Kept::default(), cx);
    };
    cx.spawn(async move |cx| {
        let kept = cx
            .background_executor()
            .spawn(async move { Kept::read(&path) })
            .await;
        cx.update(|cx| learn(&kept, cx)).ok();
    })
    .detach();
}

/// The set changed: it is written once it has been still for a moment.
fn changed(cx: &mut App) {
    let windows = cx.global::<Windows>();
    if windows.writer.is_none() || !windows.learned {
        return;
    }
    let settle = cx.spawn(async move |cx| {
        cx.background_executor().timer(SETTLE).await;
        cx.update(|cx| {
            let windows = cx.global::<Windows>();
            if let Some(writer) = &windows.writer {
                let _ = writer.send((windows.kept(), None));
            }
        })
        .ok();
    });
    cx.global_mut::<Windows>().settle = Some(settle);
}

/// A window was made for `root`. Its project is the last one opened, and
/// from here on the window says when it is moved or sized.
pub fn opened(root: &Path, handle: WindowHandle<Workspace>, cx: &mut App) {
    if !cx.has_global::<Windows>() {
        return;
    }
    let id = handle.window_id();
    let place = handle
        .update(cx, |_, window, cx| {
            cx.observe_window_bounds(window, move |workspace, window, cx| {
                let place = Place::at(workspace.root(cx), window, cx);
                let windows = cx.global_mut::<Windows>();
                let known = windows.open.iter_mut().find(|(known, _)| *known == id);
                if let Some((_, known)) = known
                    && *known != place
                {
                    *known = place;
                    changed(cx);
                }
            })
            .detach();
            Place::at(root.to_path_buf(), window, cx)
        })
        .unwrap_or_else(|_| Place::of(root.to_path_buf()));
    let alive: Vec<WindowId> = cx.windows().iter().map(|w| w.window_id()).collect();
    let windows = cx.global_mut::<Windows>();
    // The window that closed last was kept to come back to. With a
    // window open again, it is one that was closed.
    windows
        .open
        .retain(|(known, _)| *known != id && alive.contains(known));
    windows.open.push((id, place));
    remember(&mut windows.recent, root);
    changed(cx);
}

/// The projects opened before, the last one first, with the ones that
/// have a window now.
pub fn recent(cx: &App) -> Vec<PathBuf> {
    cx.try_global::<Windows>()
        .map(|windows| windows.recent.clone())
        .unwrap_or_default()
}

/// Forgets a project opened before: its folder is gone, or the user
/// took it off the list.
pub fn forget(root: &Path, cx: &mut App) {
    if cx.has_global::<Windows>() {
        cx.global_mut::<Windows>()
            .recent
            .retain(|known| known != root);
        changed(cx);
    }
}

/// The window `root` has, if it has one.
pub fn window_of(root: &Path, cx: &App) -> Option<WindowHandle<Workspace>> {
    let windows = cx.try_global::<Windows>()?;
    let (id, _) = windows.open.iter().find(|(_, place)| place.root == root)?;
    cx.windows()
        .into_iter()
        .find(|window| window.window_id() == *id)?
        .downcast::<Workspace>()
}

/// Shows `root` in a window: the one it has, brought to the front, or a
/// new one.
pub fn open(root: PathBuf, cx: &mut App) -> WindowHandle<Workspace> {
    let root = root.canonicalize().unwrap_or(root);
    if let Some(window) = window_of(&root, cx) {
        window
            .update(cx, |_, window, _| window.activate_window())
            .ok();
        return window;
    }
    crate::open_workspace_window(Place::of(root), None, cx)
}

/// Recent SSH projects reconnect before any tab or recovery file is read.
/// A local project takes the existing path without probing for metadata.
pub fn open_project(root: PathBuf, cx: &mut App) {
    if let Some(root) = crate::remote::cache_root(&root) {
        open_cached(Place::of(root), cx);
    } else {
        open(root, cx);
    }
}

pub(crate) fn open_cached(place: Place, cx: &mut App) {
    open_cached_file(place, None, cx);
}

/// Make the destination before closing a standalone entry window. A blocked
/// window owns neither session reads nor recovery writes while SSH connects.
pub(crate) fn cached_window(
    place: Place,
    file: &Option<PathBuf>,
    cx: &mut App,
) -> (WindowHandle<Workspace>, bool) {
    let connected = crate::remote::mount(&place.root)
        .ok()
        .flatten()
        .is_some_and(|mount| mount.is_connected());
    if !connected {
        crate::remote::mark_closed(place.root.clone());
    }
    let window =
        window_of(&place.root, cx).unwrap_or_else(|| crate::open_workspace_window(place, None, cx));
    window
        .update(cx, |workspace, window, cx| {
            window.activate_window();
            if connected {
                if let Some(file) = file {
                    workspace.open_path(file.clone(), None, window, cx);
                }
            } else {
                workspace.show_remote_error(crate::lang::t("Connecting to SSH").into(), cx);
            }
        })
        .ok();
    (window, !connected)
}

pub(crate) fn open_cached_file(place: Place, file: Option<PathBuf>, cx: &mut App) {
    let root = place.root.clone();
    let (window, reconnect) = cached_window(place, &file, cx);
    if !reconnect {
        return;
    }
    cx.spawn(async move |cx| {
        let prepared = cx
            .background_executor()
            .spawn(async move { crate::workspace::remote_workspace::prepare_cached(&root) })
            .await;
        // The original window may have been closed while connecting. A reply
        // must never bring it back or create a second destination.
        window
            .update(cx, |workspace, window, cx| match prepared {
                Ok(Some(mount)) => {
                    workspace.retain_remote(mount, cx);
                    workspace.resume_remote_session(window, cx);
                    if let Some(file) = file {
                        workspace.open_path(file, None, window, cx);
                    }
                }
                Ok(None) => {}
                Err(error) => workspace
                    .show_remote_error(crate::lang::tf("SSH connection failed: {}", &[&error]), cx),
            })
            .ok();
    })
    .detach();
}

/// Opens the windows an earlier run was left with, the ones whose
/// folders are still there, and says how many that was.
pub fn restore(kept: &Kept, cx: &mut App) -> usize {
    let mut opened = Vec::new();
    for place in &kept.open {
        // Asked before a window is on screen, like the file named on the
        // command line: this is what the first frame shows.
        let cached = crate::workspace::remote_workspace::own_cache_root(&place.root);
        if (cached || place.root.is_dir()) && !opened.contains(&place.root) {
            if crate::workspace::remote_workspace::own_cache_root(&place.root) {
                open_cached(place.clone(), cx);
            } else {
                crate::open_workspace_window(place.clone(), None, cx);
            }
            opened.push(place.root.clone());
        }
    }
    opened.len()
}

/// Keeps nothing from here on. The end of a test may not wait for the
/// thread that writes, as the end of a run does.
#[cfg(test)]
pub fn stop(cx: &mut App) {
    cx.global_mut::<Windows>().writer = None;
}

#[cfg(test)]
pub fn open_roots(cx: &App) -> Vec<PathBuf> {
    let windows = cx.global::<Windows>();
    windows
        .open
        .iter()
        .map(|(_, place)| place.root.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_project_opened_is_first_and_named_once() {
        let mut recent = Vec::new();
        for name in ["a", "b", "a", "c"] {
            remember(&mut recent, Path::new(name));
        }
        assert_eq!(recent, ["c", "a", "b"].map(PathBuf::from));
        for number in 0..RECENT * 2 {
            remember(&mut recent, Path::new(&number.to_string()));
        }
        assert_eq!(recent.len(), RECENT);
        assert_eq!(recent[0], PathBuf::from((RECENT * 2 - 1).to_string()));
    }

    #[test]
    fn a_file_that_is_not_ours_is_no_set_of_windows() {
        let dir = db::testing::dir("windows-read");
        let path = dir.join("windows.json");
        assert_eq!(Kept::read(&path), Kept::default());
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Kept::read(&path), Kept::default());
        std::fs::write(&path, r#"{"version":2,"open":[],"recent":["/a"]}"#).unwrap();
        assert_eq!(Kept::read(&path), Kept::default());
        // What an older build wrote of a window still opens it.
        std::fs::write(
            &path,
            r#"{"version":1,"open":[{"root":"/a"}],"recent":["/a","/b"]}"#,
        )
        .unwrap();
        let kept = Kept::read(&path);
        assert_eq!(kept.open, [Place::of("/a".into())]);
        assert_eq!(kept.recent, ["/a", "/b"].map(PathBuf::from));
    }

    #[test]
    fn a_window_off_what_is_left_of_the_screen_does_not_fit() {
        let display = size(px(1440.), px(900.));
        assert!(fits(display, [100., 80., 1280., 720.]));
        // Half of it over the right edge can still be taken hold of.
        assert!(fits(display, [1000., 80., 1280., 720.]));
        assert!(!fits(display, [1400., 80., 1280., 720.]));
        assert!(!fits(display, [100., 880., 1280., 720.]));
        assert!(!fits(display, [100., -40., 1280., 720.]));
        assert!(!fits(display, [-1270., 80., 1280., 720.]));
        assert!(!fits(display, [100., 80., 20., 20.]));
    }
}
