//! The workspace's view of its git repository: status, refreshed after
//! changes on disk, and the operations the UI runs, all off the UI thread.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gpui::{Context, EventEmitter, SharedString, Task};

use crate::git::{self, Repo, RepoStatus};

pub enum GitStoreEvent {
    /// Status (and so the index) may have changed.
    StatusChanged,
}

impl EventEmitter<GitStoreEvent> for GitStore {}

/// Coloring for the file tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tint {
    Modified,
    Added,
    Conflict,
}

pub struct GitStore {
    repo: Option<Repo>,
    discovered: bool,
    status: Arc<RepoStatus>,
    refresh_task: Option<Task<()>>,
    /// The last failed operation, shown in the git panel until the next success.
    pub last_error: Option<SharedString>,
}

impl GitStore {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            let repo = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root) })
                .await;
            this.update(cx, |this, cx| {
                this.repo = repo;
                this.discovered = true;
                this.refresh_now(cx);
            })
            .ok();
        })
        .detach();
        Self {
            repo: None,
            discovered: false,
            status: Arc::default(),
            refresh_task: None,
            last_error: None,
        }
    }

    pub fn repo(&self) -> Option<&Repo> {
        self.repo.as_ref()
    }

    /// True once discovery finished and found no repository.
    pub fn is_not_a_repo(&self) -> bool {
        self.discovered && self.repo.is_none()
    }

    pub fn status(&self) -> &Arc<RepoStatus> {
        &self.status
    }

    /// Refreshes after a short pause, so a burst of file events costs one
    /// `git status`.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.schedule_refresh(Duration::from_millis(100), cx);
    }

    pub fn refresh_now(&mut self, cx: &mut Context<Self>) {
        self.schedule_refresh(Duration::ZERO, cx);
    }

    fn schedule_refresh(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            cx.notify();
            return;
        };
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let status = cx
                .background_executor()
                .spawn(async move { repo.status() })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(status) = status
                    && *this.status != status
                {
                    this.status = Arc::new(status);
                    cx.emit(GitStoreEvent::StatusChanged);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Runs a git operation in the background, then refreshes status.
    /// Errors land in `last_error`; the task resolves to success.
    pub fn run(
        &mut self,
        op: impl FnOnce(&Repo) -> git::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let Some(repo) = self.repo.clone() else {
            return Task::ready(false);
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { op(&repo) })
                .await;
            this.update(cx, |this, cx| {
                let ok = result.is_ok();
                this.last_error = result.err().map(|e| e.0.into());
                this.refresh_now(cx);
                ok
            })
            .unwrap_or(false)
        })
    }

    /// Initializes a repository in `root` (the panel's empty state offers it).
    pub fn init(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let repo = cx
                .background_executor()
                .spawn(async move {
                    let ok = std::process::Command::new("git")
                        .arg("-C")
                        .arg(&root)
                        .args(["init", "-q"])
                        .status()
                        .is_ok_and(|s| s.success());
                    ok.then(|| Repo::discover(&root)).flatten()
                })
                .await;
            this.update(cx, |this, cx| {
                this.repo = repo;
                this.refresh_now(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The staged text of a file, or `None` when git does not track it.
    pub fn load_base(&self, path: &Path, cx: &mut Context<Self>) -> Task<Option<String>> {
        let Some(repo) = self.repo.clone() else {
            return Task::ready(None);
        };
        let Some(rel) = repo.relative(path) else {
            return Task::ready(None);
        };
        cx.background_executor()
            .spawn(async move { repo.index_text(&rel) })
    }

    /// Tints for the file tree: each changed file, and every folder above it
    /// with the strongest tint among its contents.
    pub fn tints(&self) -> HashMap<PathBuf, Tint> {
        let mut tints = HashMap::new();
        let Some(repo) = &self.repo else {
            return tints;
        };
        for f in &self.status.files {
            let tint = if f.conflicted {
                Tint::Conflict
            } else if f.untracked || f.staged == Some(git::Change::Added) {
                Tint::Added
            } else {
                Tint::Modified
            };
            let path = repo.workdir.join(&f.path);
            for p in path
                .ancestors()
                .take_while(|p| p.starts_with(&repo.workdir))
            {
                let entry = tints.entry(p.to_path_buf()).or_insert(tint);
                *entry = (*entry).max(tint);
            }
        }
        tints
    }
}
