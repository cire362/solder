//! Local models: what the machine can run, a catalog of models, downloading
//! llama.cpp's server and model files, running the server and measuring how
//! fast it is. No UI.
//!
//! Network work runs on a small Tokio runtime started by the first call, so
//! the editor pays nothing until AI is used. Everything that touches the
//! disk or spawns processes blocks: call it from a background executor.

pub mod bench;
pub mod catalog;
pub mod context;
pub mod custom;
pub mod gguf;
pub mod hardware;
pub mod install;
pub mod keys;
pub mod provider;
pub mod server;

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

pub use bench::Speed;
pub use catalog::{Candidate, Model, Role, Source};
pub use hardware::Hardware;
pub use server::LocalServer;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("solder-ai")
            .enable_all()
            .build()
            .expect("AI runtime")
    })
}

/// No overall timeout: model files take minutes. Stalls are caught by the
/// read timeout instead.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .user_agent(concat!("Solder/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client")
    })
}

/// Runs `future` on the AI runtime; the result can be awaited from any
/// executor.
pub fn spawn<T: Send + 'static>(
    future: impl Future<Output = Result<T, String>> + Send + 'static,
) -> impl Future<Output = Result<T, String>> + Send + 'static {
    let handle = runtime().spawn(future);
    async move { handle.await.map_err(|e| e.to_string())? }
}

/// Where downloads stand, shared with the UI and readable at any time.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    cancelled: AtomicBool,
}

impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// 0.0 to 1.0, or `None` before the size is known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| self.done.load(Ordering::Relaxed) as f32 / total as f32)
    }
}

/// Where Solder keeps the server and models: large, so not in the config
/// directory, and shared by every project.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub root: PathBuf,
}

impl Dirs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn runtime(&self) -> PathBuf {
        self.root.join("llama")
    }

    pub fn models(&self) -> PathBuf {
        self.root.join("models")
    }

    pub fn model(&self, model: &Model) -> PathBuf {
        match &model.source {
            Source::Hub { file, .. } => self.models().join(file),
            Source::File(path) => path.clone(),
        }
    }

    pub fn state(&self) -> PathBuf {
        self.root.join("ai.json")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
}

/// `1.2 GB`, `640 MB`.
pub fn format_size(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.0} MB", b / 1_000_000.)
    }
}

/// Bytes free on the volume holding `path` (or its nearest existing parent).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut dir = path;
    while !dir.exists() {
        dir = dir.parent()?;
    }
    hardware::free_space(dir)
}
