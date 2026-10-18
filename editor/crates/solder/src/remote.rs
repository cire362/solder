//! Project IO and process launch on another machine or in a container.
//!
//! The paths views own stay local and stable. A registered project resolves
//! them on its target; a closed target fails rather than using stale cache
//! bytes. Every IO operation here blocks and belongs off the UI thread.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, SystemTime},
};

const WORKER: &str = include_str!("remote/worker.py");
const HEADER_LIMIT: u64 = 16 * 1024 * 1024;
const FILE_LIMIT: usize = 512 * 1024 * 1024;
const WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Endpoint {
    Ssh {
        host: String,
        port: u16,
    },
    Container {
        program: PathBuf,
        id: String,
        user: Option<String>,
        #[serde(skip)]
        env: Vec<(String, String)>,
    },
    #[cfg(test)]
    Process {
        program: PathBuf,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Target {
    pub endpoint: Endpoint,
    pub path: String,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
fn closed() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        "The project connection is closed",
    )
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

impl Endpoint {
    pub fn validate(&self) -> io::Result<()> {
        match self {
            Self::Ssh { host, port } => {
                if *port == 0
                    || host.is_empty()
                    || host.starts_with('-')
                    || !host
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"@._:-[]%".contains(&c))
                {
                    return Err(invalid("Invalid SSH host or port"));
                }
            }
            Self::Container { id, user, env, .. } => {
                if id.is_empty()
                    || id.starts_with('-')
                    || !id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                {
                    return Err(invalid("Invalid container identifier"));
                }
                if user.as_ref().is_some_and(|user| {
                    user.is_empty()
                        || !user
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_:-".contains(&c))
                }) {
                    return Err(invalid("Invalid container user"));
                }
                for (key, value) in env {
                    check_env(key, value)?;
                }
            }
            #[cfg(test)]
            Self::Process { .. } => {}
        }
        Ok(())
    }

    /// Builds argv; neither connects nor runs a program.
    pub fn command(
        &self,
        cwd: &str,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        tty: bool,
    ) -> io::Result<Command> {
        self.validate()?;
        if cwd.contains('\0')
            || program.is_empty()
            || program.contains('\0')
            || args.iter().any(|arg| arg.contains('\0'))
        {
            return Err(invalid("Invalid process argument"));
        }
        for (key, value) in env {
            check_env(key, value)?;
        }
        Ok(match self {
            Self::Ssh { host, port } => {
                let mut cmd = Command::new("ssh");
                cmd.args([
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "StrictHostKeyChecking=yes",
                    "-o",
                    "ConnectTimeout=10",
                    "-o",
                    "ServerAliveInterval=15",
                    "-o",
                    "ServerAliveCountMax=2",
                ]);
                cmd.arg(if tty { "-tt" } else { "-T" })
                    .arg("-p")
                    .arg(port.to_string())
                    .arg("--")
                    .arg(host);
                let mut shell = format!("cd -- {} && exec ", quoted(cwd));
                if !env.is_empty() {
                    shell.push_str("env ");
                    for (key, value) in env {
                        shell.push_str(&quoted(&format!("{key}={value}")));
                        shell.push(' ');
                    }
                }
                shell.push_str(&quoted(program));
                for arg in args {
                    shell.push(' ');
                    shell.push_str(&quoted(arg));
                }
                cmd.arg(shell);
                cmd
            }
            Self::Container {
                program: docker,
                id,
                user,
                env: configured,
                ..
            } => {
                let mut cmd = Command::new(docker);
                cmd.args(["exec", "-i"]);
                if tty {
                    cmd.arg("-t");
                }
                cmd.arg("-w").arg(cwd);
                if let Some(user) = user {
                    cmd.arg("-u").arg(user);
                }
                for (key, value) in configured.iter().chain(env) {
                    cmd.arg("-e").arg(format!("{key}={value}"));
                }
                cmd.arg(id).arg(program).args(args);
                cmd
            }
            #[cfg(test)]
            Self::Process { program: python } => {
                // The same helper runs directly in fixture processes.
                if program == "python3"
                    && args
                        .windows(2)
                        .any(|pair| pair[0] == "-c" && pair[1] == WORKER)
                {
                    let mut cmd = Command::new(python);
                    cmd.args(["-I", "-u", "-c"]).arg(WORKER).arg(cwd);
                    cmd
                } else {
                    let mut cmd = Command::new(program);
                    cmd.args(args)
                        .envs(env.iter().map(|(k, v)| (k, v)))
                        .current_dir(cwd);
                    cmd
                }
            }
        })
    }
}

fn check_env(key: &str, value: &str) -> io::Result<()> {
    if key.is_empty()
        || key.as_bytes()[0].is_ascii_digit()
        || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        || value.contains('\0')
    {
        return Err(invalid("Invalid environment variable"));
    }
    Ok(())
}

struct Reply {
    result: Value,
    bytes: Vec<u8>,
}
type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<io::Result<Reply>>>>>;
struct Session {
    input: mpsc::Sender<(Vec<u8>, Vec<u8>)>,
    pending: Pending,
    next: AtomicU64,
    open: Arc<AtomicBool>,
    stop: mpsc::Sender<()>,
}
impl Session {
    fn start(mut cmd: Command) -> io::Result<Arc<Self>> {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take().ok_or_else(closed)?;
        let output = child.stdout.take().ok_or_else(closed)?;
        let errors = child.stderr.take().ok_or_else(closed)?;
        let pending: Pending = Arc::default();
        let open = Arc::new(AtomicBool::new(true));
        let stderr = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (stop, stopped) = mpsc::channel();
        let (writer, writes) = mpsc::channel::<(Vec<u8>, Vec<u8>)>();
        let writer_stop = stop.clone();
        let writer_open = open.clone();
        std::thread::Builder::new()
            .name("project-writer".into())
            .spawn(move || {
                let mut input = input;
                for (header, bytes) in writes {
                    if !writer_open.load(Ordering::Relaxed) {
                        break;
                    }
                    if input
                        .write_all(&header)
                        .and_then(|_| input.write_all(&bytes))
                        .and_then(|_| input.flush())
                        .is_err()
                    {
                        writer_open.store(false, Ordering::Relaxed);
                        writer_stop.send(()).ok();
                        break;
                    }
                }
            })?;
        let reader_pending = pending.clone();
        let reader_open = open.clone();
        let reader_errors = stderr.clone();
        std::thread::Builder::new()
            .name("project-reader".into())
            .spawn(move || {
                let mut output = BufReader::new(output);
                let why = loop {
                    let mut header = String::new();
                    let line = (&mut output).take(HEADER_LIMIT + 1).read_line(&mut header);
                    if !matches!(line, Ok(1..))
                        || header.len() as u64 > HEADER_LIMIT
                        || !header.ends_with('\n')
                    {
                        break "The project channel closed";
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&header) else {
                        break "Invalid project frame";
                    };
                    let Some(id) = value["id"].as_u64() else {
                        break "Invalid project reply";
                    };
                    let Some(length) = value["bytes"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| *n <= FILE_LIMIT)
                    else {
                        break "Invalid project payload length";
                    };
                    let mut bytes = vec![0; length];
                    if output.read_exact(&mut bytes).is_err() {
                        break "The project channel closed inside a file";
                    }
                    let result = if let Some(error) = value["error"].as_str() {
                        let kind = if value["conflict"].as_bool() == Some(true) {
                            io::ErrorKind::WouldBlock
                        } else {
                            match value["code"].as_i64() {
                                Some(2) => io::ErrorKind::NotFound,
                                Some(13) => io::ErrorKind::PermissionDenied,
                                Some(17) => io::ErrorKind::AlreadyExists,
                                _ => io::ErrorKind::Other,
                            }
                        };
                        Err(io::Error::new(kind, error.to_string()))
                    } else {
                        Ok(Reply {
                            result: value["result"].clone(),
                            bytes,
                        })
                    };
                    if let Some(reply) = lock(&reader_pending).remove(&id) {
                        reply.send(result).ok();
                    }
                };
                reader_open.store(false, Ordering::Relaxed);
                let detail = String::from_utf8_lossy(&lock(&reader_errors))
                    .trim()
                    .to_string();
                for (_, sender) in lock(&reader_pending).drain() {
                    sender
                        .send(Err(io::Error::new(
                            io::ErrorKind::NotConnected,
                            format!("{why}: {detail}"),
                        )))
                        .ok();
                }
            })?;
        std::thread::Builder::new()
            .name("project-stderr".into())
            .spawn(move || {
                let mut errors = errors;
                let mut part = [0; 1024];
                while let Ok(n @ 1..) = errors.read(&mut part) {
                    let mut kept = lock(&stderr);
                    kept.extend_from_slice(&part[..n]);
                    if kept.len() > 8192 {
                        let extra = kept.len() - 8192;
                        kept.drain(..extra);
                    }
                }
            })?;
        let supervisor_open = open.clone();
        std::thread::Builder::new()
            .name("project-process".into())
            .spawn(move || {
                loop {
                    match stopped.recv_timeout(Duration::from_secs(1)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if child.try_wait().ok().flatten().is_some() {
                        supervisor_open.store(false, Ordering::Relaxed);
                        return;
                    }
                }
                child.kill().ok();
                child.wait().ok();
                supervisor_open.store(false, Ordering::Relaxed);
            })?;
        Ok(Arc::new(Self {
            input: writer,
            pending,
            next: AtomicU64::new(1),
            open,
            stop,
        }))
    }
    fn call(&self, method: &str, params: Value, bytes: &[u8]) -> io::Result<Reply> {
        if !self.open.load(Ordering::Relaxed) {
            return Err(closed());
        }
        if bytes.len() > FILE_LIMIT {
            return Err(invalid("The file is too large"));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lock(&self.pending).insert(id, tx);
        let header = json!({"id": id, "method": method, "params": params, "bytes": bytes.len()});
        let mut header = serde_json::to_vec(&header).map_err(io::Error::other)?;
        header.push(b'\n');
        if self.input.send((header, bytes.to_vec())).is_err() {
            lock(&self.pending).remove(&id);
            return Err(closed());
        }
        match rx.recv_timeout(WAIT) {
            Ok(result) => result,
            Err(_) => {
                lock(&self.pending).remove(&id);
                self.open.store(false, Ordering::Relaxed);
                self.stop.send(()).ok();
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "The project stopped answering",
                ))
            }
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop.send(()).ok();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stamp {
    pub length: u64,
    pub mtime: u64,
    #[serde(default)]
    pub hash: Option<String>,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Entry {
    path: String,
    kind: String,
    length: u64,
    mtime: u64,
    #[serde(default)]
    ignored: bool,
}
#[derive(Clone, Debug)]
pub struct Metadata {
    length: u64,
    kind: String,
    mtime: u64,
}
impl Metadata {
    pub fn len(&self) -> u64 {
        self.length
    }
    pub fn is_file(&self) -> bool {
        self.kind == "file"
    }
    pub fn is_dir(&self) -> bool {
        self.kind == "dir"
    }
    pub fn is_symlink(&self) -> bool {
        self.kind == "link"
    }
    pub fn modified(&self) -> io::Result<SystemTime> {
        Ok(SystemTime::UNIX_EPOCH + Duration::from_nanos(self.mtime))
    }
}

struct Registered {
    root: PathBuf,
    mount: Weak<Mount>,
}
fn registry() -> &'static Mutex<Vec<Registered>> {
    static REGISTRY: OnceLock<Mutex<Vec<Registered>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}
/// Recognize a cache even before its connection has registered this run.
/// Descendants remain remote too, including paths the IO boundary will reject.
pub fn cache_root_under(settings: &Path, path: &Path) -> Option<PathBuf> {
    let base = settings.join("remote/ssh");
    let mut parts = path.strip_prefix(&base).ok()?.components();
    let Component::Normal(key) = parts.next()? else {
        return None;
    };
    let key = key.to_str()?;
    if key.len() != 64
        || !key
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || parts.next()? != Component::Normal(std::ffi::OsStr::new("project"))
    {
        return None;
    }
    Some(base.join(key).join("project"))
}
pub fn cache_root(path: &Path) -> Option<PathBuf> {
    cache_root_under(&crate::settings::config_dir(), path)
}
fn lookup(path: &Path) -> Option<io::Result<Arc<Mount>>> {
    lookup_under(&crate::settings::config_dir(), path)
}
fn lookup_under(settings: &Path, path: &Path) -> Option<io::Result<Arc<Mount>>> {
    lock(registry())
        .iter()
        .filter(|entry| path.starts_with(&entry.root))
        .max_by_key(|entry| entry.root.components().count())
        .map(|entry| entry.mount.upgrade().ok_or_else(closed))
        .or_else(|| cache_root_under(settings, path).map(|_| Err(closed())))
}
pub fn is_remote(path: &Path) -> bool {
    lookup(path).is_some()
}

/// Blocking: a native file dialog can return another spelling of the cache
/// (on macOS, /tmp instead of /private/tmp). Stop resolving at the remote
/// boundary so links inside a project still pass through its confinement.
pub fn save_path(path: &Path) -> io::Result<PathBuf> {
    save_path_under(&crate::settings::config_dir(), path)
}

fn save_path_under(settings: &Path, path: &Path) -> io::Result<PathBuf> {
    if lookup_under(settings, path).is_some() {
        return Ok(path.to_path_buf());
    }
    let mut prefix = PathBuf::new();
    let mut parts = path.components();
    while let Some(part) = parts.next() {
        prefix.push(part);
        prefix = match prefix.canonicalize() {
            Ok(physical) => physical,
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        };
        if lookup_under(settings, &prefix).is_some() {
            prefix.extend(parts);
            return Ok(prefix);
        }
    }
    // Local Save As keeps its symlink identity and metadata rules.
    Ok(path.to_path_buf())
}
/// A mapped URI must still be checked before an editor follows it.
pub fn contains(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
        && path.strip_prefix(root).is_ok_and(|relative| {
            relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        })
}

pub struct Mount {
    pub root: PathBuf,
    pub target: Target,
    mirror: bool,
    original: PathBuf,
    session: Option<Arc<Session>>,
    entries: Mutex<HashMap<String, Entry>>,
    versions: Mutex<HashMap<String, Stamp>>,
    git: Mutex<Option<String>>,
    changes: Mutex<std::collections::HashSet<PathBuf>>,
    git_changed: AtomicBool,
}
impl Mount {
    /// Creates the authenticated channel and its local path namespace. Blocking.
    pub fn connect(mut target: Target, root: PathBuf, mirror: bool) -> io::Result<Arc<Self>> {
        target.endpoint.validate()?;
        if !target.path.starts_with('/') || target.path.contains('\0') {
            return Err(invalid("Expected a project directory"));
        }
        std::fs::create_dir_all(&root)?;
        let shared = !mirror && matches!(target.endpoint, Endpoint::Container { .. });
        let session = if shared {
            None
        } else {
            let session = Session::start(target.endpoint.command(
                &target.path,
                "python3",
                &[
                    "-I".into(),
                    "-u".into(),
                    "-c".into(),
                    WORKER.into(),
                    target.path.clone(),
                ],
                &[],
                false,
            )?)?;
            let hello = session.call("hello", json!({}), &[])?;
            if hello.result["version"].as_u64() != Some(1) {
                return Err(invalid("Unsupported project protocol"));
            }
            target.path = hello.result["root"]
                .as_str()
                .filter(|p| p.starts_with('/'))
                .ok_or_else(|| invalid("Invalid project directory"))?
                .to_string();
            Some(session)
        };
        let mount = Arc::new(Self {
            root: root.canonicalize()?,
            target,
            mirror,
            original: root,
            session,
            entries: Mutex::default(),
            versions: Mutex::default(),
            git: Mutex::default(),
            changes: Mutex::default(),
            git_changed: AtomicBool::new(false),
        });
        mount.refresh(&mount.root)?;
        let mut entries = lock(registry());
        entries.retain(|r| r.root != mount.root && r.root != mount.original);
        entries.push(Registered {
            root: mount.root.clone(),
            mount: Arc::downgrade(&mount),
        });
        if mount.original != mount.root {
            entries.push(Registered {
                root: mount.original.clone(),
                mount: Arc::downgrade(&mount),
            });
        }
        Ok(mount)
    }
    fn call(&self, method: &str, params: Value, bytes: &[u8]) -> io::Result<Reply> {
        self.session
            .as_ref()
            .ok_or_else(closed)?
            .call(method, params, bytes)
    }
    pub fn is_mirror(&self) -> bool {
        self.mirror
    }
    pub fn is_connected(&self) -> bool {
        self.session
            .as_ref()
            .is_none_or(|session| session.open.load(Ordering::Acquire))
    }
    fn shared_path(&self, path: &Path) -> io::Result<PathBuf> {
        let relative = self.relative(path)?;
        let mut current = self.root.clone();
        for component in Path::new(&relative).components() {
            current.push(component.as_os_str());
            match std::fs::symlink_metadata(&current) {
                Ok(m) if m.is_symlink() => {
                    return Err(invalid("Symlinks are not supported in remote projects"));
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(current)
    }
    fn relative(&self, path: &Path) -> io::Result<String> {
        let relative = path
            .strip_prefix(&self.root)
            .or_else(|_| path.strip_prefix(&self.original))
            .map_err(|_| invalid("The path leaves the project"))?;
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(invalid("The path leaves the project"));
        }
        relative
            .to_str()
            .map(|p| p.replace(std::path::MAIN_SEPARATOR, "/"))
            .ok_or_else(|| invalid("The path is not UTF-8"))
    }
    pub fn remote_path(&self, path: &Path) -> io::Result<String> {
        let relative = self.relative(path)?;
        Ok(if relative.is_empty() {
            self.target.path.clone()
        } else {
            format!("{}/{}", self.target.path.trim_end_matches('/'), relative)
        })
    }
    pub fn local_path(&self, remote: &str) -> Option<PathBuf> {
        let suffix = if remote == self.target.path {
            ""
        } else {
            remote.strip_prefix(&format!("{}/", self.target.path.trim_end_matches('/')))?
        };
        if Path::new(suffix)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return None;
        }
        Some(self.root.join(suffix))
    }
    pub fn refresh(&self, scope: &Path) -> io::Result<Vec<PathBuf>> {
        let relative = self.relative(scope)?;
        if !self.mirror {
            self.shared_path(scope)?;
            return Ok(Vec::new());
        }
        let result = self.call("list", json!({"path": relative}), &[])?.result;
        if result["limited"].as_bool() == Some(true) {
            return Err(io::Error::other("The project has more than 50000 entries"));
        }
        let entries: Vec<Entry> =
            serde_json::from_value(result["entries"].clone()).map_err(io::Error::other)?;
        let mut changed = Vec::new();
        let mut known = lock(&self.entries);
        let mut seen = std::collections::HashSet::new();
        for entry in entries {
            let local = self.shared_path(&self.root.join(&entry.path))?;
            self.relative(&local)?;
            if !local.starts_with(scope) || !matches!(entry.kind.as_str(), "file" | "dir") {
                return Err(invalid("Invalid project entry"));
            }
            seen.insert(entry.path.clone());
            if known.get(&entry.path) != Some(&entry) {
                changed.push(local.clone());
            }
            if self.mirror {
                if entry.kind == "dir" {
                    std::fs::create_dir_all(&local)?;
                } else {
                    if let Some(parent) = local.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    if !local.exists() {
                        std::fs::File::create(&local)?;
                    }
                }
            }
            known.insert(entry.path.clone(), entry);
        }
        let removed: Vec<_> = known
            .keys()
            .filter(|p| {
                self.root.join(p).starts_with(scope)
                    && !(relative.is_empty() && p.starts_with(".solder/"))
                    && !seen.contains(*p)
            })
            .cloned()
            .collect();
        for path in removed {
            known.remove(&path);
            let local = self.shared_path(&self.root.join(&path))?;
            changed.push(local.clone());
            if self.mirror {
                if local.is_dir() {
                    std::fs::remove_dir_all(&local).ok();
                } else {
                    std::fs::remove_file(&local).ok();
                }
            }
        }
        let git = result["git"].as_str().map(str::to_string);
        let mut old_git = lock(&self.git);
        if *old_git != git {
            self.git_changed.store(true, Ordering::Release);
        }
        *old_git = git;
        lock(&self.changes).extend(changed.iter().cloned());
        Ok(changed)
    }
    /// Only the project's watcher drains this. A scan made by an agent
    /// cannot consume file changes before the window has heard them.
    pub fn take_changes(&self) -> Vec<PathBuf> {
        lock(&self.changes).drain().collect()
    }

    pub fn take_git_change(&self) -> bool {
        self.git_changed.swap(false, Ordering::AcqRel)
    }

    pub fn files(&self, scope: &Path) -> Vec<PathBuf> {
        lock(&self.entries)
            .values()
            .filter(|e| e.kind == "file" && !e.ignored)
            .map(|e| self.root.join(&e.path))
            .filter(|p| p.starts_with(scope))
            .collect()
    }
    pub fn search(
        &self,
        scope: &Path,
        query: &str,
        case: bool,
        word: bool,
        regex: bool,
    ) -> io::Result<Value> {
        Ok(self.call("search", json!({"path":self.relative(scope)?,"query":query,"case":case,"word":word,"regex":regex}), &[])?.result)
    }
    pub fn which(&self, program: &str) -> io::Result<Option<String>> {
        Ok(self
            .call("which", json!({"program":program}), &[])?
            .result
            .as_str()
            .map(str::to_string))
    }
    fn cache(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let checked = self.shared_path(path)?;
        let path = checked.as_path();
        if self.mirror && std::fs::read(path).ok().as_deref() != Some(bytes) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, bytes)?;
        }
        Ok(())
    }
}

/// Keep failed SSH namespaces closed while their window reports the error.
/// Old cache bytes must never become local source files after a failed reconnect.
pub fn mark_closed(root: PathBuf) {
    let mut registered = lock(registry());
    // A failed concurrent reconnect must not replace one that succeeded.
    if registered.iter().any(|entry| {
        entry.root == root
            && entry
                .mount
                .upgrade()
                .is_some_and(|mount| mount.is_connected())
    }) {
        return;
    }
    registered.retain(|entry| entry.root != root);
    registered.push(Registered {
        root,
        mount: Weak::new(),
    });
}

pub fn mount(path: &Path) -> io::Result<Option<Arc<Mount>>> {
    lookup(path).transpose()
}
pub fn remote_path(path: &Path) -> Option<String> {
    lookup(path)?.ok()?.remote_path(path).ok()
}
pub fn local_path(root: &Path, remote: &str) -> Option<PathBuf> {
    lookup(root)?.ok()?.local_path(remote)
}
pub fn command(
    cwd: &Path,
    program: &str,
    args: &[String],
    env: &[(String, String)],
) -> io::Result<Command> {
    match mount(cwd)? {
        Some(mount) => {
            let mut cmd = mount.target.endpoint.command(
                &mount.remote_path(cwd)?,
                program,
                args,
                env,
                false,
            )?;
            #[cfg(test)]
            let fixture = matches!(mount.target.endpoint, Endpoint::Process { .. });
            #[cfg(not(test))]
            let fixture = false;
            if !fixture {
                cmd.current_dir(&mount.root);
            }
            Ok(cmd)
        }
        None => {
            let mut cmd = Command::new(program);
            cmd.current_dir(cwd)
                .args(args)
                .envs(env.iter().map(|(k, v)| (k, v)));
            Ok(cmd)
        }
    }
}
pub fn sync_tree(root: &Path) -> io::Result<()> {
    if let Some(mount) = mount(root)? {
        mount.refresh(root)?;
    }
    Ok(())
}
impl Drop for Mount {
    fn drop(&mut self) {
        if !self.mirror {
            let this = self as *const Self;
            lock(registry()).retain(|entry| entry.mount.as_ptr() != this);
        }
    }
}

pub fn stamp_for_bytes(path: &Path, bytes: &[u8]) -> io::Result<Stamp> {
    let m = std::fs::metadata(path)?;
    let mtime = m
        .modified()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64;
    Ok(Stamp {
        length: bytes.len() as u64,
        mtime,
        hash: Some(ai::install::sha256_hex(bytes)),
    })
}

pub fn read_versioned(path: &Path) -> io::Result<(Vec<u8>, Option<Stamp>)> {
    match mount(path)? {
        None => {
            let bytes = std::fs::read(path)?;
            let stamp = stamp_for_bytes(path, &bytes)?;
            Ok((bytes, Some(stamp)))
        }
        Some(mount) if !mount.mirror => {
            let path = mount.shared_path(path)?;
            let bytes = std::fs::read(&path)?;
            let stamp = stamp_for_bytes(&path, &bytes)?;
            Ok((bytes, Some(stamp)))
        }
        Some(mount) => {
            mount.shared_path(path)?;
            let reply = mount.call("read", json!({"path": mount.relative(path)?}), &[])?;
            let stamp: Stamp = serde_json::from_value(reply.result).map_err(io::Error::other)?;
            mount.cache(path, &reply.bytes)?;
            lock(&mount.versions).insert(mount.relative(path)?, stamp.clone());
            Ok((reply.bytes, Some(stamp)))
        }
    }
}
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    if !is_remote(path) {
        return std::fs::read(path);
    }
    Ok(read_versioned(path)?.0)
}
pub fn read_to_string(path: &Path) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
pub fn write_versioned(
    path: &Path,
    bytes: &[u8],
    expected: Option<&Stamp>,
) -> io::Result<Option<Stamp>> {
    let resolved = save_path(path)?;
    let path = resolved.as_path();
    match mount(path)? {
        None => crate::file_save::write(path, bytes, expected).map(Some),
        Some(mount) if !mount.mirror => {
            crate::file_save::write(&mount.shared_path(path)?, bytes, expected).map(Some)
        }
        Some(mount) => {
            // Refuse a linked cache destination before changing the target.
            mount.shared_path(path)?;
            let expected = expected
                .map(|s| json!(s))
                .unwrap_or_else(|| json!({"missing":true}));
            let reply = mount.call(
                "write",
                json!({"path":mount.relative(path)?,"expected":expected}),
                bytes,
            )?;
            let stamp: Stamp = serde_json::from_value(reply.result).map_err(io::Error::other)?;
            mount.cache(path, bytes)?;
            lock(&mount.versions).insert(mount.relative(path)?, stamp.clone());
            Ok(Some(stamp))
        }
    }
}
pub fn write(path: &Path, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    let resolved = save_path(path)?;
    let path = resolved.as_path();
    match mount(path)? {
        None => return crate::file_save::replace(path, bytes.as_ref()).map(|_| ()),
        Some(mount) if !mount.mirror => {
            return crate::file_save::replace(&mount.shared_path(path)?, bytes.as_ref())
                .map(|_| ());
        }
        _ => {}
    }
    let version = mount(path)?.and_then(|m| {
        lock(&m.versions)
            .get(&m.relative(path).ok()?)
            .cloned()
            .or_else(|| {
                lock(&m.entries)
                    .get(&m.relative(path).ok()?)
                    .map(|e| Stamp {
                        length: e.length,
                        mtime: e.mtime,
                        hash: None,
                    })
            })
    });
    write_versioned(path, bytes.as_ref(), version.as_ref()).map(|_| ())
}
/// Captures an explicitly chosen Save As destination off the UI thread.
pub fn revision(path: &Path) -> io::Result<Option<Stamp>> {
    let resolved = save_path(path)?;
    let path = resolved.as_path();
    match mount(path)? {
        None => crate::file_save::revision(path),
        Some(mount) if !mount.mirror => crate::file_save::revision(&mount.shared_path(path)?),
        Some(_) => match read_versioned(path) {
            Ok((_, stamp)) => Ok(stamp),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        },
    }
}

pub fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    let (resolved_a, resolved_b) = (save_path(a)?, save_path(b)?);
    let (a, b) = (resolved_a.as_path(), resolved_b.as_path());
    if a == b {
        return Ok(true);
    }
    // A disconnected virtual source cannot alias a destination outside the
    // remote namespace. Save As must still be able to rescue its open text.
    let ma = match mount(a) {
        Err(error) if error.kind() == io::ErrorKind::NotConnected && !is_remote(b) => {
            return Ok(false);
        }
        other => other?,
    };
    let (ma, mb) = (ma, mount(b)?);
    match (ma, mb) {
        (None, None) => crate::file_save::same_file(a, b),
        (Some(ma), Some(mb)) if ma.mirror || mb.mirror => {
            Ok(Arc::ptr_eq(&ma, &mb) && ma.relative(a)? == mb.relative(b)?)
        }
        (Some(ma), Some(mb)) => {
            crate::file_save::same_file(&ma.shared_path(a)?, &mb.shared_path(b)?)
        }
        (Some(mount), None) if !mount.mirror => {
            crate::file_save::same_file(&mount.shared_path(a)?, b)
        }
        (None, Some(mount)) if !mount.mirror => {
            crate::file_save::same_file(a, &mount.shared_path(b)?)
        }
        _ => Ok(false),
    }
}

pub fn metadata(path: &Path) -> io::Result<Metadata> {
    match mount(path)? {
        None => {
            let m = std::fs::metadata(path)?;
            let mtime = m
                .modified()?
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .min(u64::MAX as u128) as u64;
            Ok(Metadata {
                length: m.len(),
                kind: if m.is_symlink() {
                    "link"
                } else if m.is_dir() {
                    "dir"
                } else {
                    "file"
                }
                .into(),
                mtime,
            })
        }
        Some(mount) if !mount.mirror => {
            let path = mount.shared_path(path)?;
            let m = std::fs::metadata(path)?;
            let mtime = m
                .modified()?
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .min(u64::MAX as u128) as u64;
            Ok(Metadata {
                length: m.len(),
                kind: if m.is_dir() { "dir" } else { "file" }.into(),
                mtime,
            })
        }
        Some(mount) => {
            let entry: Entry = serde_json::from_value(
                mount
                    .call("stat", json!({"path":mount.relative(path)?}), &[])?
                    .result,
            )
            .map_err(io::Error::other)?;
            Ok(Metadata {
                length: entry.length,
                kind: entry.kind,
                mtime: entry.mtime,
            })
        }
    }
}
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    match mount(path)? {
        None => std::fs::canonicalize(path),
        Some(mount) => {
            mount.relative(path)?;
            if metadata(path)?.is_symlink() {
                return Err(invalid("Symlinks are not supported in remote projects"));
            }
            Ok(path.to_path_buf())
        }
    }
}
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    match mount(path)? {
        None => std::fs::create_dir_all(path),
        Some(mount) if !mount.mirror => std::fs::create_dir_all(mount.shared_path(path)?),
        Some(mount) => {
            mount.call("mkdir", json!({"path":mount.relative(path)?}), &[])?;
            if mount.mirror {
                std::fs::create_dir_all(path)?;
            }
            Ok(())
        }
    }
}
fn remove(path: &Path, recursive: bool) -> io::Result<()> {
    match mount(path)? {
        None => {
            if recursive {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
        }
        Some(mount) if !mount.mirror => {
            let path = mount.shared_path(path)?;
            if recursive {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
        }
        Some(mount) => {
            mount.call(
                "remove",
                json!({"path":mount.relative(path)?,"recursive":recursive}),
                &[],
            )?;
            if mount.mirror && path.exists() {
                if recursive {
                    std::fs::remove_dir_all(path)?;
                } else {
                    std::fs::remove_file(path)?;
                }
            }
            Ok(())
        }
    }
}
pub fn remove_file(path: &Path) -> io::Result<()> {
    remove(path, false)
}
pub fn remove_dir_all(path: &Path) -> io::Result<()> {
    remove(path, true)
}
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    match (mount(from)?, mount(to)?) {
        (None, None) => std::fs::rename(from, to),
        (Some(mount), Some(other)) if Arc::ptr_eq(&mount, &other) && !mount.mirror => {
            std::fs::rename(mount.shared_path(from)?, mount.shared_path(to)?)
        }
        (Some(mount), Some(other)) if Arc::ptr_eq(&mount, &other) => {
            mount.call(
                "rename",
                json!({"path":mount.relative(from)?,"to":mount.relative(to)?}),
                &[],
            )?;
            if mount.mirror && from.exists() {
                std::fs::rename(from, to)?;
            }
            Ok(())
        }
        _ => Err(invalid("Cannot move a file between project environments")),
    }
}

#[cfg(test)]
pub(crate) fn fixture(source: &Path, cache: &Path) -> Arc<Mount> {
    Mount::connect(
        Target {
            endpoint: Endpoint::Process {
                program: "python3".into(),
            },
            path: source.to_str().unwrap().into(),
        },
        cache.to_path_buf(),
        true,
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    struct Folder(PathBuf);
    impl Folder {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "solder-remote-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Folder {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_cache_descendant_stays_closed_before_registration() {
        let settings = Path::new("/settings");
        let root = settings
            .join("remote/ssh")
            .join("a".repeat(64))
            .join("project");
        for path in [
            &root,
            &root.join("src/main.rs"),
            &root.join("../connection.json"),
        ] {
            assert_eq!(cache_root_under(settings, path), Some(root.clone()));
            assert_eq!(
                lookup_under(settings, path).unwrap().err().unwrap().kind(),
                io::ErrorKind::NotConnected
            );
        }
        for path in [
            settings.join("remote/ssh/a/project"),
            root.parent().unwrap().to_path_buf(),
            PathBuf::from("/project/src/main.rs"),
        ] {
            assert!(cache_root_under(settings, &path).is_none());
            assert!(lookup_under(settings, &path).is_none());
        }
    }

    #[test]
    fn endpoints_keep_arguments_as_data_and_refuse_control_arguments() {
        let ssh = Endpoint::Ssh {
            host: "user@example".into(),
            port: 2222,
        };
        let cmd = ssh
            .command(
                "/srv/a b",
                "python3",
                &["x'; touch /tmp/never".into()],
                &[("TOKEN".into(), "a' b".into())],
                false,
            )
            .unwrap();
        let args: Vec<_> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"StrictHostKeyChecking=yes".into()));
        assert!(args.last().unwrap().contains("'x'\\''; touch /tmp/never'"));
        assert!(args.last().unwrap().contains("'TOKEN=a'\\'' b'"));
        assert!(
            Endpoint::Ssh {
                host: "-oProxyCommand=evil".into(),
                port: 22
            }
            .validate()
            .is_err()
        );
        assert!(
            Endpoint::Container {
                program: "docker".into(),
                id: "--privileged".into(),
                user: None,
                env: vec![]
            }
            .validate()
            .is_err()
        );
        assert!(
            ssh.command(
                "/srv/project",
                "sh",
                &[],
                &[("X=bad".into(), "data".into())],
                false
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn bytes_are_preserved_and_conflicts_do_not_overwrite_the_server() {
        let folder = Folder::new();
        let source = folder.0.join("server");
        let cache = folder.0.join("cache");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("file.bin"), [0xff, 0xfe, b'A', 0, 0x80]).unwrap();
        let mount = fixture(&source, &cache);
        let path = cache.join("file.bin");
        let (bytes, stamp) = read_versioned(&path).unwrap();
        assert_eq!(bytes, [0xff, 0xfe, b'A', 0, 0x80]);
        write_versioned(&path, &[0xff, 0xfe, b'B', 0, 0x80], stamp.as_ref()).unwrap();
        assert_eq!(
            std::fs::read(source.join("file.bin")).unwrap(),
            [0xff, 0xfe, b'B', 0, 0x80]
        );
        let (_, stamp) = read_versioned(&path).unwrap();
        let stamp = stamp.unwrap();
        std::fs::write(source.join("file.bin"), [0xff, 0xfe, b'C', 0, 0x80]).unwrap();
        std::fs::File::open(source.join("file.bin"))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_nanos(stamp.mtime))
            .unwrap();
        assert!(write_versioned(&path, b"changed", Some(&stamp)).is_err());
        assert_eq!(
            std::fs::read(source.join("file.bin")).unwrap(),
            [0xff, 0xfe, b'C', 0, 0x80]
        );
        drop(mount);
        assert!(is_remote(&path));
        assert_eq!(read(&path).unwrap_err().kind(), io::ErrorKind::NotConnected);
        assert!(write(&path, b"local fallback").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn save_as_aliases_stay_remote_and_keep_conflicts_and_closed_channels() {
        let folder = Folder::new();
        let source = folder.0.join("server");
        let cache = folder.0.join("cache");
        let alias = folder.0.join("dialog-alias");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("file.txt"), "server").unwrap();
        let mount = fixture(&source, &cache);
        std::os::unix::fs::symlink(&cache, &alias).unwrap();
        std::fs::write(cache.join("file.txt"), "stale cache").unwrap();
        assert_eq!(
            save_path(&alias.join("new.txt")).unwrap(),
            cache.join("new.txt")
        );
        assert!(same_file(&cache.join("file.txt"), &alias.join("file.txt")).unwrap());
        let original = revision(&alias.join("file.txt")).unwrap().unwrap();
        assert_eq!(original.length, 6);
        write_versioned(&alias.join("new.txt"), b"rescued", None).unwrap();
        assert_eq!(std::fs::read(source.join("new.txt")).unwrap(), b"rescued");
        std::fs::write(source.join("file.txt"), "external").unwrap();
        assert!(write_versioned(&alias.join("file.txt"), b"stale", Some(&original)).is_err());
        assert_eq!(std::fs::read(source.join("file.txt")).unwrap(), b"external");

        // Resolve an outside alias only up to the project, never through an
        // interior link that the remote file rules must refuse.
        std::os::unix::fs::symlink(&source, cache.join("escape")).unwrap();
        let escape = alias.join("escape/file.txt");
        assert_eq!(save_path(&escape).unwrap(), cache.join("escape/file.txt"));
        assert!(revision(&escape).is_err());
        assert!(write_versioned(&escape, b"escape", None).is_err());
        assert_eq!(std::fs::read(source.join("file.txt")).unwrap(), b"external");

        drop(mount);
        assert_eq!(
            revision(&alias.join("new.txt")).unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
        assert!(write_versioned(&alias.join("closed.txt"), b"local fallback", None).is_err());
        assert!(write(&alias.join("new.txt"), b"local fallback").is_err());
        assert_eq!(std::fs::read(cache.join("new.txt")).unwrap(), b"rescued");
        assert!(!cache.join("closed.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn dialog_aliases_of_unregistered_caches_are_recognized_and_local_links_are_kept() {
        let folder = Folder::new();
        let settings = folder.0.join("settings");
        let root = settings
            .join("remote/ssh")
            .join("a".repeat(64))
            .join("project");
        std::fs::create_dir_all(&root).unwrap();
        let alias = folder.0.join("settings-alias");
        std::os::unix::fs::symlink(&settings, &alias).unwrap();
        let path = alias
            .join("remote/ssh")
            .join("a".repeat(64))
            .join("project/new.txt");
        let resolved = save_path_under(&settings, &path).unwrap();
        assert_eq!(resolved, root.join("new.txt"));
        assert_eq!(
            lookup_under(&settings, &resolved)
                .unwrap()
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::NotConnected
        );
        let local = folder.0.join("local.txt");
        std::fs::write(&local, "local").unwrap();
        let linked = folder.0.join("local-link.txt");
        std::os::unix::fs::symlink(&local, &linked).unwrap();
        assert_eq!(save_path(&linked).unwrap(), linked);
    }

    #[cfg(unix)]
    #[test]
    fn a_disconnected_source_does_not_prevent_saving_as_a_local_file() {
        let folder = Folder::new();
        let source = folder.0.join("server");
        let cache = folder.0.join("cache");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("file.txt"), "source").unwrap();
        let mount = fixture(&source, &cache);
        let path = cache.join("file.txt");
        read_versioned(&path).unwrap();
        drop(mount);
        let target = folder.0.join("local.txt");
        assert!(is_remote(&path));
        assert!(!same_file(&path, &target).unwrap());
        assert!(same_file(&path, &path).unwrap());
        let stamp = revision(&target).unwrap();
        write_versioned(&target, b"rescued", stamp.as_ref()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"rescued");
        assert!(read(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn project_operations_cannot_follow_links_or_escape_the_root() {
        let folder = Folder::new();
        let source = folder.0.join("server");
        let cache = folder.0.join("cache");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(folder.0.join("outside"), b"secret").unwrap();
        std::os::unix::fs::symlink(&folder.0, source.join("link")).unwrap();
        let _mount = fixture(&source, &cache);
        assert!(read(&cache.join("../outside")).is_err());
        assert!(read(&cache.join("link/outside")).is_err());
        assert!(write(&cache.join("link/outside"), b"bad").is_err());
        create_dir_all(&cache.join("src")).unwrap();
        write(&cache.join("src/new.rs"), b"fn main() {}").unwrap();
        rename(&cache.join("src/new.rs"), &cache.join("src/renamed.rs")).unwrap();
        assert_eq!(
            read(&cache.join("src/renamed.rs")).unwrap(),
            b"fn main() {}"
        );
        remove_dir_all(&cache.join("src")).unwrap();
        assert!(!source.join("src").exists());
        assert_eq!(std::fs::read(folder.0.join("outside")).unwrap(), b"secret");
    }

    #[test]
    fn bind_mounts_use_host_files_and_route_processes_without_python() {
        let folder = Folder::new();
        let path = folder.0.join("file.txt");
        std::fs::write(&path, b"before").unwrap();
        let mount = Mount::connect(
            Target {
                endpoint: Endpoint::Container {
                    program: "docker-fixture".into(),
                    id: "owned-123".into(),
                    user: Some("1000".into()),
                    env: vec![("MODE".into(), "test".into())],
                },
                path: "/workspace/project".into(),
            },
            folder.0.clone(),
            false,
        )
        .unwrap();
        let (bytes, stamp) = read_versioned(&path).unwrap();
        assert_eq!(bytes, b"before");
        write_versioned(&path, b"saved", stamp.as_ref()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"saved");
        assert!(write_versioned(&path, b"stale", stamp.as_ref()).is_err());
        assert!(read(&folder.0.join("../outside")).is_err());
        let cmd = command(&folder.0, "git", &["status".into()], &[]).unwrap();
        let argv = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            argv,
            [
                "exec",
                "-i",
                "-w",
                "/workspace/project",
                "-u",
                "1000",
                "-e",
                "MODE=test",
                "owned-123",
                "git",
                "status"
            ]
        );
        assert_eq!(
            remote_path(&path).as_deref(),
            Some("/workspace/project/file.txt")
        );
        drop(mount);
        assert!(!is_remote(&path));
    }

    #[cfg(unix)]
    #[test]
    fn a_project_cannot_shadow_the_helpers_standard_library() {
        let folder = Folder::new();
        let source = folder.0.join("server");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(
            source.join("json.py"),
            "raise RuntimeError('project code must not run')",
        )
        .unwrap();
        std::fs::write(source.join("file.txt"), "ordinary data").unwrap();
        let cache = folder.0.join("cache");
        let _mount = fixture(&source, &cache);
        assert_eq!(read(&cache.join("file.txt")).unwrap(), b"ordinary data");
        assert!(!source.join("__pycache__").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_background_inventory_cannot_consume_the_watchers_changes() {
        let folder = Folder::new();
        let source = folder.0.join("server");
        let cache = folder.0.join("cache");
        std::fs::create_dir(&source).unwrap();
        let mount = fixture(&source, &cache);
        mount.take_changes();
        std::fs::write(source.join("new.txt"), b"new").unwrap();
        // A context collection may refresh before the next watcher poll.
        sync_tree(&cache).unwrap();
        mount.refresh(&mount.root).unwrap();
        assert_eq!(mount.take_changes(), [mount.root.join("new.txt")]);
        assert!(mount.take_changes().is_empty());
        let outside = folder.0.join("outside");
        std::fs::write(&outside, b"private").unwrap();
        std::fs::remove_file(cache.join("new.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, cache.join("new.txt")).unwrap();
        assert!(read(&cache.join("new.txt")).is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"private");
    }

    #[cfg(unix)]
    #[test]
    fn commands_run_in_the_target_directory_not_the_cache() {
        let folder = Folder::new();
        let source = folder.0.join("server");
        let cache = folder.0.join("cache");
        std::fs::create_dir(&source).unwrap();
        let _mount = fixture(&source, &cache);
        let out = command(
            &cache,
            "/bin/sh",
            &["-c".into(), "pwd; printf '%s' \"$CHECK\"".into()],
            &[("CHECK".into(), "remote".into())],
        )
        .unwrap()
        .output()
        .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert_eq!(text, format!("{}\nremote", source.display()));
    }
}
