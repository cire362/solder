//! The open folder: its file list (for the finder and search) and a watcher
//! that reports changes on disk.

use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::StreamExt;
use gpui::{Context, EventEmitter, Task};
use notify::{EventKind, RecursiveMode, Watcher};

pub enum ProjectEvent {
    /// Files whose contents or existence changed on disk.
    Changed(Vec<PathBuf>),
    /// Something under `.git` that affects status changed: the index, HEAD
    /// or a ref (a commit, checkout, stage from the command line).
    GitChanged,
    /// The file list was rebuilt.
    Scanned,
}

pub struct Project {
    root: PathBuf,
    // The registry keeps weak references; the open project owns the connection.
    mount: Option<Arc<crate::remote::Mount>>,
    generation: u64,
    inventory: Arc<Mutex<()>>,
    error: Option<String>,
    watch_error: Option<String>,
    /// Paths relative to the root, `/`-separated, sorted.
    files: Arc<Vec<Arc<str>>>,
    scanning: bool,
    scan_task: Option<Task<()>>,
    _watch_task: Option<Task<()>>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl EventEmitter<ProjectEvent> for Project {}

/// Directories never worth scanning or watching.
pub fn is_excluded_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".next" | "dist" | ".turbo"
    )
}

impl Project {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut project = Self {
            mount: crate::remote::mount(&root).ok().flatten(),
            root,
            generation: 0,
            inventory: Arc::default(),
            error: None,
            watch_error: None,
            files: Arc::default(),
            scanning: false,
            scan_task: None,
            _watch_task: None,
            _watcher: None,
        };
        project.rescan(cx);
        project.watch(cx);
        project
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn mount(&self) -> Option<&Arc<crate::remote::Mount>> {
        self.mount.as_ref()
    }

    /// Attach an environment to an already open shared folder. No IO here.
    pub fn attach_remote(
        &mut self,
        mount: Arc<crate::remote::Mount>,
        cx: &mut Context<Self>,
    ) -> io::Result<()> {
        mount.remote_path(&self.root)?;
        self.generation = self.generation.wrapping_add(1);
        self.scan_task = None;
        self._watch_task = None;
        self._watcher = None;
        self.watch_error = None;
        self.mount = Some(mount);
        self.rescan(cx);
        self.watch(cx);
        Ok(())
    }

    /// Transport errors for the workspace's connection notice.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref().or(self.watch_error.as_deref())
    }

    pub fn files(&self) -> Arc<Vec<Arc<str>>> {
        self.files.clone()
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    pub fn relative<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        path.strip_prefix(&self.root).ok()
    }

    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.scanning = true;
        let root = self.root.clone();
        let generation = self.generation;
        let inventory = self.inventory.clone();
        self.scan_task = Some(cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move {
                    let _inventory = inventory.lock().unwrap_or_else(|e| e.into_inner());
                    scan_snapshot(&root)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match files {
                    Ok(snapshot) => {
                        this.files = Arc::new(snapshot.files);
                        this.error = None;
                        if !snapshot.changed.is_empty() {
                            cx.emit(ProjectEvent::Changed(snapshot.changed));
                        }
                        if snapshot.git_changed {
                            cx.emit(ProjectEvent::GitChanged);
                        }
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                this.scanning = false;
                cx.emit(ProjectEvent::Scanned);
            })
            .ok();
        }));
    }

    fn watch(&mut self, cx: &mut Context<Self>) {
        match crate::remote::mount(&self.root) {
            Ok(Some(mount)) if mount.is_mirror() => {
                self.watch_remote(mount, cx);
                return;
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.emit(ProjectEvent::Scanned);
                cx.notify();
                return;
            }
            _ => {}
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<notify::Event>();
        let root = self.root.clone();
        let generation = self.generation;
        let sessions = crate::settings::config_dir().join("sessions");
        self._watch_task = Some(cx.spawn(async move |this, cx| {
            let watcher = cx
                .background_executor()
                .spawn(async move {
                    let mut watcher = notify::recommended_watcher(
                        move |event: notify::Result<notify::Event>| {
                            if let Ok(event) = event {
                                tx.unbounded_send(event).ok();
                            }
                        },
                    )?;
                    watcher.watch(&root, RecursiveMode::Recursive)?;
                    Ok::<_, notify::Error>(watcher)
                })
                .await;
            let watcher = match watcher {
                Ok(watcher) => watcher,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        this.watch_error = Some(error.to_string());
                        cx.emit(ProjectEvent::Scanned);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            if this
                .update(cx, |this, _| {
                    if this.generation != generation {
                        return false;
                    }
                    this._watcher = Some(watcher);
                    true
                })
                .ok()
                != Some(true)
            {
                return;
            }
            while let Some(first) = rx.next().await {
                // Coalesce bursts (a branch switch, a formatter run) into one update.
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                let mut events = vec![first];
                while let Ok(e) = rx.try_recv() {
                    events.push(e);
                }
                let mut changed = HashSet::new();
                let mut structural = false;
                let mut git_changed = false;
                for mut e in events {
                    if matches!(e.kind, EventKind::Access(_)) {
                        continue;
                    }
                    // Opening HOME can put our state inside the project.
                    // Atomic cursor saves must not trigger another full scan.
                    without_session_paths(&mut e, &sessions);
                    if e.paths.is_empty() {
                        continue;
                    }
                    structural |= matches!(e.kind, EventKind::Create(_) | EventKind::Remove(_))
                        || matches!(
                            e.kind,
                            EventKind::Modify(notify::event::ModifyKind::Name(_))
                        );
                    for p in e.paths {
                        if is_git_state(&p) {
                            git_changed = true;
                            continue;
                        }
                        let excluded = p
                            .components()
                            .any(|c| c.as_os_str().to_str().is_some_and(is_excluded_dir));
                        if !excluded {
                            changed.insert(p);
                        }
                    }
                }
                if changed.is_empty() && !git_changed {
                    continue;
                }
                let result = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    if structural && !changed.is_empty() {
                        this.rescan(cx);
                    }
                    if !changed.is_empty() {
                        cx.emit(ProjectEvent::Changed(changed.into_iter().collect()));
                    }
                    if git_changed {
                        cx.emit(ProjectEvent::GitChanged);
                    }
                });
                if result.is_err() {
                    break;
                }
            }
        }));
    }

    fn watch_remote(&mut self, mount: Arc<crate::remote::Mount>, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let generation = self.generation;
        let inventory = self.inventory.clone();
        self._watch_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let result = cx
                    .background_executor()
                    .spawn({
                        let (root, mount, inventory) =
                            (root.clone(), mount.clone(), inventory.clone());
                        async move {
                            let _inventory = inventory.lock().unwrap_or_else(|e| e.into_inner());
                            remote_snapshot(&root, &mount)
                        }
                    })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        match result {
                            Ok(snapshot) => {
                                let recovered = this.error.take().is_some();
                                if *this.files != snapshot.files {
                                    this.files = Arc::new(snapshot.files);
                                    cx.emit(ProjectEvent::Scanned);
                                }
                                if !snapshot.changed.is_empty() {
                                    cx.emit(ProjectEvent::Changed(snapshot.changed));
                                }
                                if snapshot.git_changed {
                                    cx.emit(ProjectEvent::GitChanged);
                                }
                                if recovered {
                                    cx.emit(ProjectEvent::Scanned);
                                    cx.notify();
                                }
                            }
                            Err(error) => {
                                let error = error.to_string();
                                if this.error.as_ref() != Some(&error) {
                                    this.error = Some(error);
                                    cx.emit(ProjectEvent::Scanned);
                                    cx.notify();
                                }
                            }
                        }
                        true
                    })
                    .ok()
                    != Some(true)
                {
                    break;
                }
            }
        }));
    }
}

struct RemoteSnapshot {
    files: Vec<Arc<str>>,
    changed: Vec<PathBuf>,
    git_changed: bool,
}

fn remote_scope(root: &Path, mount: &crate::remote::Mount) -> io::Result<PathBuf> {
    mount
        .local_path(&mount.remote_path(root)?)
        .ok_or_else(|| io::ErrorKind::InvalidInput.into())
}

fn remote_files(scope: &Path, mount: &crate::remote::Mount) -> Vec<Arc<str>> {
    let mut files: Vec<Arc<str>> = mount
        .files(scope)
        .into_iter()
        .filter_map(|path| {
            let relative = path.strip_prefix(scope).ok()?;
            if relative.starts_with(".solder/worktrees") {
                return None;
            }
            if relative
                .components()
                .any(|part| part.as_os_str().to_str().is_some_and(is_excluded_dir))
                || relative.file_name().is_some_and(|name| name == ".DS_Store")
            {
                return None;
            }
            Some(Arc::from(relative.to_str()?.replace('\\', "/")))
        })
        .collect();
    files.sort_unstable();
    files
}

fn remote_snapshot(root: &Path, mount: &crate::remote::Mount) -> io::Result<RemoteSnapshot> {
    let scope = remote_scope(root, mount)?;
    mount.refresh(&scope)?;
    let changed = mount
        .take_changes()
        .into_iter()
        .filter_map(|path| Some(root.join(path.strip_prefix(&scope).ok()?)))
        .collect();
    Ok(RemoteSnapshot {
        files: remote_files(&scope, mount),
        changed,
        git_changed: mount.take_git_change(),
    })
}

/// Files inside `.git` whose changes can change `git status`.
fn is_git_state(path: &Path) -> bool {
    let mut components = path.components().map(|c| c.as_os_str().to_string_lossy());
    if !components.any(|c| c == ".git") {
        return false;
    }
    let rest: Vec<_> = components.collect();
    matches!(
        rest.first().map(|c| c.as_ref()),
        Some("index" | "HEAD" | "MERGE_HEAD" | "refs")
    )
}

/// Walks the tree in parallel, honoring `.gitignore`, `.ignore` and global
/// git excludes.
pub fn scan(root: &Path) -> Vec<Arc<str>> {
    scan_result(root).unwrap_or_default()
}

fn scan_result(root: &Path) -> io::Result<Vec<Arc<str>>> {
    scan_snapshot(root).map(|snapshot| snapshot.files)
}

fn scan_snapshot(root: &Path) -> io::Result<RemoteSnapshot> {
    match crate::remote::mount(root)? {
        Some(mount) if mount.is_mirror() => remote_snapshot(root, &mount),
        _ => Ok(RemoteSnapshot {
            files: scan_with_sessions(root, crate::settings::config_dir().join("sessions")),
            changed: Vec::new(),
            git_changed: false,
        }),
    }
}

fn without_session_paths(event: &mut notify::Event, sessions: &Path) {
    event.paths.retain(|path| !path.starts_with(sessions));
}

fn scan_with_sessions(root: &Path, sessions: PathBuf) -> Vec<Arc<str>> {
    let files = Mutex::new(Vec::new());
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(move |e| {
            let name = e.file_name().to_str().unwrap_or("");
            !(e.path().starts_with(&sessions)
                || e.file_type().is_some_and(|t| t.is_dir()) && is_excluded_dir(name)
                || name == ".DS_Store")
        })
        .build_parallel()
        .run(|| {
            let files = &files;
            Box::new(move |entry| {
                if let Ok(entry) = entry
                    && entry.file_type().is_some_and(|t| t.is_file())
                    && let Ok(rel) = entry.path().strip_prefix(root)
                {
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    files.lock().unwrap().push(Arc::from(rel));
                }
                ignore::WalkState::Continue
            })
        });
    let mut files = files.into_inner().unwrap();
    files.sort_unstable();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn ssh_inventory_uses_target_ignores_and_polls_changes_without_reading_bytes() {
        let dir = db::testing::dir("remote-project-inventory")
            .canonicalize()
            .unwrap();
        let source = dir.join("server");
        let cache = dir.join("cache");
        std::fs::create_dir(&source).unwrap();
        assert!(
            std::process::Command::new("git")
                .arg("init")
                .arg("-q")
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(source.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(source.join("ignored.txt"), "not in the finder").unwrap();
        std::fs::write(source.join("a.txt"), "authoritative bytes").unwrap();
        let mount = crate::remote::fixture(&source, &cache);
        let files = scan_result(&cache).unwrap();
        assert_eq!(
            files.iter().map(|s| &**s).collect::<Vec<_>>(),
            [".gitignore", "a.txt"]
        );
        assert_eq!(std::fs::read(cache.join("a.txt")).unwrap(), b"");
        assert_eq!(std::fs::read(cache.join(".gitignore")).unwrap(), b"");
        std::fs::write(source.join("a.txt"), "changed on the target").unwrap();
        std::fs::write(source.join("b.txt"), "new").unwrap();
        let snapshot = remote_snapshot(&cache, &mount).unwrap();
        assert!(snapshot.changed.contains(&cache.join("a.txt")));
        assert!(snapshot.changed.contains(&cache.join("b.txt")));
        assert!(snapshot.git_changed);
        assert_eq!(std::fs::read(cache.join("a.txt")).unwrap(), b"");
        std::fs::remove_file(source.join("a.txt")).unwrap();
        let snapshot = remote_snapshot(&cache, &mount).unwrap();
        assert!(snapshot.changed.contains(&cache.join("a.txt")));
        assert_eq!(
            snapshot.files.iter().map(|s| &**s).collect::<Vec<_>>(),
            [".gitignore", "b.txt"]
        );
        drop(mount);
        assert_eq!(
            scan_result(&cache).unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
        assert!(scan(&cache).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shared_container_inventory_keeps_native_files_and_ignores() {
        let root = db::testing::dir("remote-project-shared")
            .canonicalize()
            .unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "ignored").unwrap();
        std::fs::write(root.join("source.rs"), "fn main() {}\n").unwrap();
        let mount = crate::remote::Mount::connect(
            crate::remote::Target {
                endpoint: crate::remote::Endpoint::Container {
                    program: "docker-not-run".into(),
                    id: "fixture".into(),
                    user: None,
                    env: Vec::new(),
                },
                path: "/workspace/project".into(),
            },
            root.clone(),
            false,
        )
        .unwrap();
        assert!(!mount.is_mirror());
        assert_eq!(
            scan_result(&root)
                .unwrap()
                .iter()
                .map(|s| &**s)
                .collect::<Vec<_>>(),
            [".gitignore", "source.rs"]
        );
        assert_eq!(
            std::fs::read(root.join("source.rs")).unwrap(),
            b"fn main() {}\n"
        );
        drop(mount);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ssh_inventory_maps_canonical_cache_paths_back_to_the_project_alias() {
        let root = db::testing::dir("remote-project-alias")
            .canonicalize()
            .unwrap();
        let source = root.join("server");
        let cache = root.join("cache");
        let alias = root.join("linked-cache");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&cache).unwrap();
        std::os::unix::fs::symlink(&cache, &alias).unwrap();
        let mount = crate::remote::fixture(&source, &alias);
        std::fs::write(source.join("a.txt"), "target").unwrap();
        let snapshot = remote_snapshot(&alias, &mount).unwrap();
        assert_eq!(snapshot.changed, [alias.join("a.txt")]);
        assert_eq!(
            snapshot.files.iter().map(|s| &**s).collect::<Vec<_>>(),
            ["a.txt"]
        );
        assert_eq!(
            scan_result(&alias)
                .unwrap()
                .iter()
                .map(|s| &**s)
                .collect::<Vec<_>>(),
            ["a.txt"]
        );
        drop(mount);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_saves_inside_a_project_do_not_rescan_or_enter_the_finder() {
        let dir = db::testing::dir("session-scan");
        let sessions = dir.join(".config/solder/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(sessions.join("project.json"), "{}").unwrap();
        std::fs::write(dir.join("sessions/user.json"), "{}").unwrap();
        let files = scan_with_sessions(&dir, sessions.clone());
        assert_eq!(
            files.iter().map(|file| &**file).collect::<Vec<_>>(),
            ["sessions/user.json"]
        );
        let mut event = notify::Event::new(EventKind::Create(notify::event::CreateKind::File))
            .add_path(sessions.join(".solder-save-1"))
            .add_path(sessions.join("project.json"));
        without_session_paths(&mut event, &sessions);
        assert!(event.paths.is_empty());
        event.paths.push(dir.join("sessions/user.json"));
        without_session_paths(&mut event, &sessions);
        assert_eq!(event.paths, [dir.join("sessions/user.json")]);
    }

    #[test]
    fn recognizes_git_state_files() {
        assert!(is_git_state(Path::new("/p/.git/index")));
        assert!(is_git_state(Path::new("/p/.git/refs/heads/main")));
        assert!(!is_git_state(Path::new("/p/.git/objects/ab/cd")));
        assert!(!is_git_state(Path::new("/p/src/index")));
    }

    #[test]
    fn scan_respects_gitignore_and_exclusions() {
        let dir = db::testing::dir("scan");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::create_dir(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".gitignore"), "build/\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        std::fs::write(dir.join("node_modules/pkg/index.js"), "").unwrap();
        std::fs::write(dir.join("build/out.js"), "").unwrap();
        let files: Vec<String> = scan(&dir).iter().map(|s| s.to_string()).collect();
        assert_eq!(files, vec![".gitignore", "src/main.rs"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
