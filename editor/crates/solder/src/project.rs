//! The open folder: its file list (for the finder and search) and a watcher
//! that reports changes on disk.

use std::{
    collections::HashSet,
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
            root,
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
        self.scan_task = Some(cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { scan(&root) })
                .await;
            this.update(cx, |this, cx| {
                this.files = Arc::new(files);
                this.scanning = false;
                cx.emit(ProjectEvent::Scanned);
            })
            .ok();
        }));
    }

    fn watch(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<notify::Event>();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                tx.unbounded_send(event).ok();
            }
        });
        let Ok(mut watcher) = watcher else { return };
        if watcher.watch(&self.root, RecursiveMode::Recursive).is_err() {
            return;
        }
        self._watcher = Some(watcher);
        self._watch_task = Some(cx.spawn(async move |this, cx| {
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
                for e in events {
                    if matches!(e.kind, EventKind::Access(_)) {
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
    let files = Mutex::new(Vec::new());
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            !(e.file_type().is_some_and(|t| t.is_dir()) && is_excluded_dir(name))
                && name != ".DS_Store"
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
