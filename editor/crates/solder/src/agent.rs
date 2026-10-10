//! Agent tasks: a model that plans, edits files and runs commands in a Git
//! worktree of its own, on its own branch, until the task is done. The
//! user's checkout is never touched until they merge.
//!
//! Commands run in a sandbox: no network beyond this machine and no writes
//! outside the worktree and temporary folders (`sandbox-exec` on macOS,
//! `bwrap` on Linux). A command that needs the network or the rest of the
//! disk says so and waits for the user. Without a sandbox, every command
//! waits. Everything here blocks: call it from a background executor.

use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use ai::tools::{ToolCall, ToolResult, ToolSpec};

/// What a file read returns at most.
const READ_LIMIT: usize = 100_000;
/// Command output kept: the start and the end, which hold the errors.
const OUTPUT_HEAD: usize = 2_000;
const OUTPUT_TAIL: usize = 8_000;
const SEARCH_LIMIT: usize = 100;
pub const DEFAULT_TIMEOUT: u64 = 120;
const MAX_TIMEOUT: u64 = 600;

// ------------------------------------------------------------------ git

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err
            .lines()
            .map(|l| {
                l.trim_start_matches("error: ")
                    .trim_start_matches("fatal: ")
            })
            .find(|l| !l.trim().is_empty())
            .unwrap_or("git failed")
            .to_string())
    }
}

/// `Fix the login form!` becomes `fix-the-login-form`.
pub fn slug(task: &str) -> String {
    let mut out = String::new();
    for c in task.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "task".into() } else { out }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Worktree {
    /// The user's checkout.
    pub repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    /// The commit the task started from.
    pub base: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub path: String,
    /// `A`, `M`, `D` or `R`.
    pub kind: char,
}

impl Worktree {
    /// A new branch from the user's last commit, checked out under `dir`.
    pub fn create(repo: &Path, dir: &Path, task: &str) -> Result<Self, String> {
        let repo = PathBuf::from(git(repo, &["rev-parse", "--show-toplevel"])?.trim());
        let base = git(&repo, &["rev-parse", "HEAD"])
            .map_err(|_| {
                "The project needs at least one commit for an agent to branch from".to_string()
            })?
            .trim()
            .to_string();
        let name = slug(task);
        let project = repo
            .file_name()
            .map_or("project".into(), |n| slug(&n.to_string_lossy()));
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        // Never reuse a branch or folder of an earlier task.
        let mut n = 1;
        let (branch, path) = loop {
            let suffix = if n == 1 {
                String::new()
            } else {
                format!("-{n}")
            };
            let branch = format!("solder/agent/{name}{suffix}");
            let path = dir.join(format!("{project}-{name}{suffix}"));
            let taken = git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .is_ok();
            if !taken && !path.exists() {
                break (branch, path);
            }
            n += 1;
        };
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &path.to_string_lossy(),
                &base,
            ],
        )?;
        Ok(Self {
            repo,
            path,
            branch,
            base,
        })
    }

    /// Files that differ from where the task started, untracked included.
    pub fn changes(&self) -> Result<Vec<Change>, String> {
        // Staging in the agent's own worktree is harmless and makes new
        // files show in the diff.
        git(&self.path, &["add", "-A"])?;
        let out = git(
            &self.path,
            &["diff", "--cached", "--name-status", &self.base],
        )?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let mut parts = line.split('\t');
                let kind = parts.next()?.chars().next()?;
                let path = parts.next_back()?.to_string();
                Some(Change { path, kind })
            })
            .collect())
    }

    pub fn file_at_base(&self, rel: &str) -> Option<String> {
        git(&self.path, &["show", &format!("{}:{rel}", self.base)]).ok()
    }

    pub fn file_now(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.path.join(rel)).ok()
    }

    /// Commits what the agent changed; false when nothing did.
    pub fn commit(&self, message: &str) -> Result<bool, String> {
        if self.changes()?.is_empty() {
            return Ok(false);
        }
        git(&self.path, &["commit", "-q", "-m", message])?;
        Ok(true)
    }

    /// Merges the task's branch into the user's current branch and removes
    /// the worktree. Refuses rather than touching uncommitted work that
    /// the merge would overwrite (Git's own check).
    pub fn merge(&self, message: &str) -> Result<(), String> {
        if !self.commit(message)? {
            return Err("The agent changed nothing to merge".into());
        }
        git(
            &self.repo,
            &["merge", "--no-ff", "-m", message, &self.branch],
        )
        .map_err(|e| {
            let _ = git(&self.repo, &["merge", "--abort"]);
            format!("Could not merge: {e}")
        })?;
        self.remove();
        Ok(())
    }

    /// Deletes the worktree and its branch.
    pub fn remove(&self) {
        let _ = git(
            &self.repo,
            &[
                "worktree",
                "remove",
                "--force",
                &self.path.to_string_lossy(),
            ],
        );
        let _ = git(&self.repo, &["branch", "-D", &self.branch]);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Worktrees earlier tasks left under `trees` for this repository: tasks do
/// not outlive the editor, their branches do until removed.
pub fn leftovers(repo: &Path, trees: &Path) -> Vec<Worktree> {
    let Ok(repo) = git(repo, &["rev-parse", "--show-toplevel"]).map(|r| PathBuf::from(r.trim()))
    else {
        return Vec::new();
    };
    let trees = trees.canonicalize().unwrap_or_else(|_| trees.to_path_buf());
    let Ok(list) = git(&repo, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    list.split("\n\n")
        .filter_map(|block| {
            let path = PathBuf::from(block.lines().find_map(|l| l.strip_prefix("worktree "))?);
            let branch = block
                .lines()
                .find_map(|l| l.strip_prefix("branch refs/heads/"))?
                .to_string();
            let inside = path
                .canonicalize()
                .unwrap_or_else(|_| path.clone())
                .starts_with(&trees);
            (inside && branch.starts_with("solder/agent/")).then(|| Worktree {
                repo: repo.clone(),
                path,
                branch,
                base: String::new(),
            })
        })
        .collect()
}

// -------------------------------------------------------------- sandbox

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// This machine only, writes in the worktree and temporary folders.
    Sandboxed,
    /// Also the network, and package caches in the home folder.
    Network,
    /// No limits.
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sandbox {
    MacOs,
    Bwrap,
    None,
}

impl Sandbox {
    pub fn detect() -> Self {
        if cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").is_file() {
            Sandbox::MacOs
        } else if cfg!(target_os = "linux")
            && Command::new("bwrap")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        {
            Sandbox::Bwrap
        } else {
            Sandbox::None
        }
    }
}

fn caches() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    [
        ".npm",
        ".cargo",
        ".rustup",
        ".cache",
        ".pnpm-store",
        ".yarn",
        ".bun",
        ".gradle",
        ".m2",
        "go",
        "Library/Caches",
    ]
    .iter()
    .map(|d| home.join(d))
    .collect()
}

fn temp_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/tmp"), std::env::temp_dir()];
    for d in dirs.clone() {
        if let Ok(real) = d.canonicalize() {
            dirs.push(real);
        }
    }
    dirs
}

/// The macOS sandbox profile: everything allowed but remote connections
/// (unless `network`) and writes outside `writable`.
fn macos_profile(writable: &[PathBuf], network: bool) -> String {
    let quote = |p: &Path| {
        p.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    };
    let mut profile = String::from("(version 1)\n(allow default)\n");
    if !network {
        profile.push_str("(deny network-outbound (remote ip))\n");
        profile.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
    }
    profile.push_str("(deny file-write*)\n(allow file-write*\n");
    for p in writable {
        profile.push_str(&format!("  (subpath \"{}\")\n", quote(p)));
    }
    profile.push_str("  (literal \"/dev/null\") (literal \"/dev/tty\") (regex #\"^/dev/fd/\"))\n");
    profile
}

/// `PATH` with the usual install folders: an app started from the Dock gets
/// a minimal one.
pub(crate) fn search_path() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for extra in [
        home.join(".cargo/bin"),
        home.join(".local/bin"),
        home.join("go/bin"),
        home.join(".bun/bin"),
        home.join("Library/pnpm"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    std::env::join_paths(dirs)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub struct CommandOutput {
    pub code: Option<i32>,
    pub output: String,
    pub timed_out: bool,
}

/// Runs `command` with `sh -c` in `dir`, limited by `access`.
pub fn run_command(
    sandbox: Sandbox,
    dir: &Path,
    command: &str,
    access: Access,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let dir = dir.canonicalize().map_err(|e| e.to_string())?;
    let mut writable = vec![dir.clone()];
    writable.extend(temp_dirs());
    if access == Access::Network {
        writable.extend(caches());
    }
    let mut cmd = match (sandbox, access) {
        (_, Access::Full) | (Sandbox::None, _) => {
            let mut c = Command::new("sh");
            c.args(["-c", command]);
            c
        }
        (Sandbox::MacOs, _) => {
            let mut c = Command::new("/usr/bin/sandbox-exec");
            c.arg("-p")
                .arg(macos_profile(&writable, access == Access::Network))
                .args(["sh", "-c", command]);
            c
        }
        (Sandbox::Bwrap, _) => {
            let mut c = Command::new("bwrap");
            c.args([
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--die-with-parent",
            ]);
            for p in writable.iter().filter(|p| p.exists()) {
                c.arg("--bind").arg(p).arg(p);
            }
            if access == Access::Sandboxed {
                c.arg("--unshare-net");
            }
            c.args(["sh", "-c", command]);
            c
        }
    };
    cmd.current_dir(&dir)
        .env("PATH", search_path())
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so a timeout stops what it started too.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not run the command: {e}"))?;
    // Both streams into one buffer, read on threads so neither pipe fills.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|mut stream| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        })
    })
    .collect();
    drop(tx);
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(status);
        }
        if start.elapsed() > timeout {
            timed_out = true;
            #[cfg(unix)]
            {
                // `--` matters: without it procps reads `-4724` as the
                // group `-4`, and a number starting with 1 as `-1`, which
                // is every process the user has.
                let _ = Command::new("kill")
                    .args(["-9", "--", &format!("-{}", child.id())])
                    .status();
            }
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    for r in readers {
        let _ = r.join();
    }
    let bytes: Vec<u8> = rx.try_iter().flatten().collect();
    Ok(CommandOutput {
        code: status.and_then(|s| s.code()),
        output: clip(&String::from_utf8_lossy(&bytes)),
        timed_out,
    })
}

/// Keeps the start and the end of long output.
fn clip(text: &str) -> String {
    if text.len() <= OUTPUT_HEAD + OUTPUT_TAIL {
        return text.to_string();
    }
    let head_end = (0..=OUTPUT_HEAD)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    let tail_start = (text.len() - OUTPUT_TAIL..text.len())
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(text.len());
    format!(
        "{}\n[... {} bytes cut ...]\n{}",
        &text[..head_end],
        tail_start - head_end,
        &text[tail_start..]
    )
}

// ---------------------------------------------------------------- tools

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Reading and proposing a plan; nothing changes.
    Planning,
    Working,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanStep {
    pub text: String,
    pub done: bool,
}

/// What a tool call leads to.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Result(ToolResult),
    /// A command that needs more than the sandbox gives; runs once the
    /// user agrees.
    NeedsApproval {
        command: String,
        access: Access,
        reason: String,
    },
    Plan(Vec<PlanStep>, ToolResult),
    Finished(String),
}

fn schema(props: serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({ "type": "object", "properties": props, "required": required })
}

fn spec(name: &str, description: &str, parameters: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters,
    }
}

pub fn specs(phase: Phase) -> Vec<ToolSpec> {
    let path =
        serde_json::json!({ "type": "string", "description": "Relative to the project root" });
    let mut out = vec![
        spec(
            "list_files",
            "List files and folders under a folder (default: the project root), a few levels deep.",
            schema(
                serde_json::json!({ "path": path, "depth": { "type": "integer" } }),
                &[],
            ),
        ),
        spec(
            "read_file",
            "Read a text file, with line numbers. Optionally a range of lines.",
            schema(
                serde_json::json!({ "path": path, "start_line": { "type": "integer" }, "end_line": { "type": "integer" } }),
                &["path"],
            ),
        ),
        spec(
            "search",
            "Search the project's files for a regular expression; returns matching lines with their file and line number.",
            schema(
                serde_json::json!({ "pattern": { "type": "string" }, "path": path }),
                &["pattern"],
            ),
        ),
        spec(
            "update_plan",
            "Set the plan: short steps, each marked done or not. Call it to propose the plan and whenever a step is done.",
            schema(
                serde_json::json!({ "steps": { "type": "array", "items": {
                    "type": "object",
                    "properties": { "text": { "type": "string" }, "done": { "type": "boolean" } },
                    "required": ["text"],
                }}}),
                &["steps"],
            ),
        ),
    ];
    if phase == Phase::Working {
        out.extend([
            spec(
                "write_file",
                "Create or replace a whole file.",
                schema(serde_json::json!({ "path": path, "content": { "type": "string" } }), &["path", "content"]),
            ),
            spec(
                "edit_file",
                "Replace one exact, unique piece of a file's text with new text.",
                schema(
                    serde_json::json!({ "path": path, "old": { "type": "string" }, "new": { "type": "string" } }),
                    &["path", "old", "new"],
                ),
            ),
            spec(
                "run",
                "Run a shell command in the project root, such as tests or a build. It has no network and can only write inside the project; set access to \"network\" or \"full\" (with a reason) when it needs more, and the user is asked.",
                schema(
                    serde_json::json!({
                        "command": { "type": "string" },
                        "timeout_seconds": { "type": "integer" },
                        "access": { "type": "string", "enum": ["sandboxed", "network", "full"] },
                        "reason": { "type": "string" },
                    }),
                    &["command"],
                ),
            ),
            spec(
                "finish",
                "End the task with a short summary of what changed and how it was checked.",
                schema(serde_json::json!({ "summary": { "type": "string" } }), &["summary"]),
            ),
        ]);
    }
    out
}

pub struct ToolBox {
    pub root: PathBuf,
    pub sandbox: Sandbox,
}

fn ok(call: &ToolCall, output: impl Into<String>) -> Outcome {
    Outcome::Result(ToolResult {
        id: call.id.clone(),
        output: output.into(),
        error: false,
    })
}

fn fail(call: &ToolCall, output: impl Into<String>) -> Outcome {
    Outcome::Result(ToolResult {
        id: call.id.clone(),
        output: output.into(),
        error: true,
    })
}

/// Folders an agent never needs to list or search.
fn skipped(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".next" | "dist" | "build" | ".venv" | "__pycache__"
    )
}

impl ToolBox {
    /// A path inside the worktree that the project lets AI see.
    fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        let rel = rel.trim();
        let rel = if rel.is_empty() { "." } else { rel };
        let p = Path::new(rel);
        if p.is_absolute()
            || p.components()
                .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(format!(
                "{rel} is outside the project; use a path relative to its root"
            ));
        }
        let full = self.root.join(p);
        if rel != "." && !crate::ai_context::allows_path(&self.root, &full).unwrap_or(false) {
            // `.env` files and what `.solderignore` lists stay private.
            let exists = full.exists();
            if exists || !ai::context::allows_file(p) {
                return Err(format!("{rel} is excluded from AI"));
            }
        }
        Ok(full)
    }

    pub fn execute(&self, call: &ToolCall, phase: Phase, approved: bool) -> Outcome {
        let input = &call.input;
        if let Some(e) = input["__invalid"].as_str() {
            return fail(call, e);
        }
        let s = |key: &str| input[key].as_str().unwrap_or("");
        let writes = matches!(
            call.name.as_str(),
            "write_file" | "edit_file" | "run" | "finish"
        );
        if writes && phase == Phase::Planning {
            return fail(
                call,
                "Not yet: propose the plan with update_plan and wait for approval",
            );
        }
        match call.name.as_str() {
            "list_files" => self.list(
                call,
                s("path"),
                input["depth"].as_u64().unwrap_or(2).min(4) as usize,
            ),
            "read_file" => self.read(
                call,
                s("path"),
                input["start_line"].as_u64(),
                input["end_line"].as_u64(),
            ),
            "search" => self.search(call, s("pattern"), s("path")),
            "update_plan" => {
                let steps: Vec<PlanStep> = input["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|step| {
                        let text = step["text"].as_str().or(step.as_str())?.trim().to_string();
                        (!text.is_empty()).then(|| PlanStep {
                            text,
                            done: step["done"].as_bool().unwrap_or(false),
                        })
                    })
                    .collect();
                if steps.is_empty() {
                    return fail(call, "The plan needs at least one step");
                }
                let result = ToolResult {
                    id: call.id.clone(),
                    output: match phase {
                        Phase::Planning => {
                            "Plan shown to the user. Stop here and wait for approval.".into()
                        }
                        Phase::Working => "Plan updated.".into(),
                    },
                    error: false,
                };
                Outcome::Plan(steps, result)
            }
            "write_file" => match self.resolve(s("path")) {
                Ok(path) => {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    match std::fs::write(&path, s("content")) {
                        Ok(()) => ok(
                            call,
                            format!(
                                "Wrote {} ({} lines)",
                                s("path"),
                                s("content").lines().count()
                            ),
                        ),
                        Err(e) => fail(call, e.to_string()),
                    }
                }
                Err(e) => fail(call, e),
            },
            "edit_file" => self.edit(call, s("path"), s("old"), s("new")),
            "run" => {
                let command = s("command").trim();
                if command.is_empty() {
                    return fail(call, "No command");
                }
                let access = match s("access") {
                    "network" => Access::Network,
                    "full" => Access::Full,
                    _ => Access::Sandboxed,
                };
                let needs_user = access != Access::Sandboxed || self.sandbox == Sandbox::None;
                if needs_user && !approved {
                    return Outcome::NeedsApproval {
                        command: command.to_string(),
                        access,
                        reason: s("reason").to_string(),
                    };
                }
                let timeout = input["timeout_seconds"]
                    .as_u64()
                    .unwrap_or(DEFAULT_TIMEOUT)
                    .clamp(1, MAX_TIMEOUT);
                match run_command(
                    self.sandbox,
                    &self.root,
                    command,
                    access,
                    Duration::from_secs(timeout),
                ) {
                    Ok(out) => {
                        let status = if out.timed_out {
                            format!("Stopped after {timeout} s.")
                        } else {
                            match out.code {
                                Some(code) => format!("Exit code {code}."),
                                None => "Killed by a signal.".into(),
                            }
                        };
                        let failed = out.timed_out || out.code != Some(0);
                        let output = if out.output.trim().is_empty() {
                            status
                        } else {
                            format!("{status}\n{}", out.output)
                        };
                        if failed {
                            fail(call, output)
                        } else {
                            ok(call, output)
                        }
                    }
                    Err(e) => fail(call, e),
                }
            }
            "finish" => Outcome::Finished(s("summary").to_string()),
            other => fail(call, format!("There is no tool named {other}")),
        }
    }

    fn list(&self, call: &ToolCall, rel: &str, depth: usize) -> Outcome {
        let dir = match self.resolve(rel) {
            Ok(d) => d,
            Err(e) => return fail(call, e),
        };
        let mut lines = Vec::new();
        fn walk(tb: &ToolBox, dir: &Path, prefix: &str, depth: usize, lines: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                if lines.len() >= 500 {
                    return;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                let rel = format!("{prefix}{name}");
                if skipped(&name) || tb.resolve(&rel).is_err() {
                    continue;
                }
                if e.path().is_dir() {
                    lines.push(format!("{rel}/"));
                    if depth > 1 {
                        walk(tb, &e.path(), &format!("{rel}/"), depth - 1, lines);
                    }
                } else {
                    lines.push(rel);
                }
            }
        }
        let prefix = if rel.trim().is_empty() || rel.trim() == "." {
            String::new()
        } else {
            format!("{}/", rel.trim().trim_end_matches('/'))
        };
        walk(self, &dir, &prefix, depth.max(1), &mut lines);
        if lines.is_empty() {
            ok(call, "(empty)")
        } else {
            ok(call, lines.join("\n"))
        }
    }

    fn read(&self, call: &ToolCall, rel: &str, start: Option<u64>, end: Option<u64>) -> Outcome {
        let path = match self.resolve(rel) {
            Ok(p) => p,
            Err(e) => return fail(call, e),
        };
        let text = match std::fs::read(&path) {
            Ok(bytes) if bytes.contains(&0) => {
                return fail(call, format!("{rel} is not a text file"));
            }
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => return fail(call, format!("{rel}: {e}")),
        };
        let start = start.unwrap_or(1).max(1) as usize;
        let end = end.map_or(usize::MAX, |e| e as usize);
        let mut out = String::new();
        for (i, line) in text.lines().enumerate().skip(start - 1) {
            if i + 1 > end {
                break;
            }
            if out.len() + line.len() > READ_LIMIT {
                out.push_str(&format!(
                    "[... cut at line {}; read further with start_line ...]\n",
                    i + 1
                ));
                break;
            }
            out.push_str(&format!("{:>5}  {line}\n", i + 1));
        }
        ok(
            call,
            if out.is_empty() {
                "(empty)".into()
            } else {
                out
            },
        )
    }

    fn search(&self, call: &ToolCall, pattern: &str, rel: &str) -> Outcome {
        let re = match regex::Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => return fail(call, format!("Bad pattern: {e}")),
        };
        let dir = match self.resolve(rel) {
            Ok(d) => d,
            Err(e) => return fail(call, e),
        };
        let mut hits = Vec::new();
        let walker = ignore::WalkBuilder::new(&dir)
            .hidden(false)
            .require_git(false)
            .filter_entry(|e| !skipped(&e.file_name().to_string_lossy()))
            .build();
        for entry in walker.flatten() {
            if hits.len() >= SEARCH_LIMIT {
                hits.push(format!("[stopped at {SEARCH_LIMIT} matches]"));
                break;
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&self.root) else {
                continue;
            };
            let rel = rel.to_string_lossy();
            if self.resolve(&rel).is_err() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    let line: String = line.chars().take(200).collect();
                    hits.push(format!("{rel}:{}: {}", i + 1, line.trim()));
                    if hits.len() >= SEARCH_LIMIT {
                        break;
                    }
                }
            }
        }
        ok(
            call,
            if hits.is_empty() {
                "No matches".into()
            } else {
                hits.join("\n")
            },
        )
    }

    fn edit(&self, call: &ToolCall, rel: &str, old: &str, new: &str) -> Outcome {
        let path = match self.resolve(rel) {
            Ok(p) => p,
            Err(e) => return fail(call, e),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => return fail(call, format!("{rel}: {e}")),
        };
        if old.is_empty() {
            return fail(call, "old is empty; use write_file to create a file");
        }
        match text.matches(old).count() {
            0 => fail(
                call,
                format!("The old text is not in {rel}; read the file again"),
            ),
            1 => match std::fs::write(&path, text.replacen(old, new, 1)) {
                Ok(()) => ok(call, format!("Edited {rel}")),
                Err(e) => fail(call, e.to_string()),
            },
            n => fail(
                call,
                format!("The old text is in {rel} {n} times; include more lines to make it unique"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> PathBuf {
        let dir = db::testing::dir(name);
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&dir)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join(".env"), "SECRET=1\n").unwrap();
        std::fs::write(dir.join(".gitignore"), ".env\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        dir.canonicalize().unwrap()
    }

    fn call(name: &str, input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: name.into(),
            input,
        }
    }

    fn output(o: Outcome) -> (String, bool) {
        match o {
            Outcome::Result(r) => (r.output, r.error),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn worktrees_branch_change_merge_and_go() {
        let repo = repo("agent-worktree");
        let dir = repo.parent().unwrap().join("agent-worktree-trees");
        let wt = Worktree::create(&repo, &dir, "Fix the add function!").unwrap();
        assert_eq!(wt.branch, "solder/agent/fix-the-add-function");
        assert!(wt.path.join("src/lib.rs").is_file());
        // The ignored .env did not come along.
        assert!(!wt.path.join(".env").exists());
        let again = Worktree::create(&repo, &dir, "Fix the add function!").unwrap();
        assert_eq!(again.branch, "solder/agent/fix-the-add-function-2");
        again.remove();
        assert!(!again.path.exists());

        std::fs::write(
            wt.path.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        )
        .unwrap();
        std::fs::write(wt.path.join("src/new.rs"), "// new\n").unwrap();
        let changes = wt.changes().unwrap();
        assert_eq!(
            changes,
            [
                Change {
                    path: "src/lib.rs".into(),
                    kind: 'M'
                },
                Change {
                    path: "src/new.rs".into(),
                    kind: 'A'
                },
            ]
        );
        assert!(wt.file_at_base("src/lib.rs").unwrap().contains("a - b"));
        assert!(wt.file_now("src/lib.rs").unwrap().contains("a + b"));
        // The user's checkout is untouched until the merge.
        assert!(
            std::fs::read_to_string(repo.join("src/lib.rs"))
                .unwrap()
                .contains("a - b")
        );
        wt.merge("Fix the add function").unwrap();
        assert!(
            std::fs::read_to_string(repo.join("src/lib.rs"))
                .unwrap()
                .contains("a + b")
        );
        assert!(repo.join("src/new.rs").is_file());
        assert!(!wt.path.exists());
        let log = git(&repo, &["log", "--oneline", "-1"]).unwrap();
        assert!(log.contains("Fix the add function"), "{log}");
        assert!(
            git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "refs/heads/solder/agent/fix-the-add-function"
                ]
            )
            .is_err()
        );
        assert_eq!(slug("  !!"), "task");

        // Tasks gone with an earlier run leave worktrees to clean up.
        let left = Worktree::create(&repo, &dir, "Left behind").unwrap();
        let found = leftovers(&repo, &dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].branch, left.branch);
        found[0].remove();
        assert!(leftovers(&repo, &dir).is_empty() && !left.path.exists());
    }

    #[test]
    fn tools_read_search_edit_and_keep_secrets() {
        let root = repo("agent-tools");
        let tb = ToolBox {
            root: root.clone(),
            sandbox: Sandbox::None,
        };
        let (listing, _) = output(tb.execute(
            &call("list_files", serde_json::json!({})),
            Phase::Planning,
            false,
        ));
        assert!(
            listing.contains("src/lib.rs")
                && !listing.contains(".env")
                && !listing.contains(".git/"),
            "{listing}"
        );
        let (text, _) = output(tb.execute(
            &call(
                "read_file",
                serde_json::json!({"path": "src/lib.rs", "start_line": 2, "end_line": 2}),
            ),
            Phase::Planning,
            false,
        ));
        assert_eq!(text, "    2      a - b\n");
        let (hits, _) = output(tb.execute(
            &call("search", serde_json::json!({"pattern": "a - b"})),
            Phase::Planning,
            false,
        ));
        assert_eq!(hits, "src/lib.rs:2: a - b");
        for path in [".env", "../outside.rs", "/etc/hosts"] {
            let (e, error) = output(tb.execute(
                &call("read_file", serde_json::json!({"path": path})),
                Phase::Planning,
                false,
            ));
            assert!(error, "{path}: {e}");
        }
        // Nothing changes while planning.
        let (e, error) = output(tb.execute(
            &call(
                "edit_file",
                serde_json::json!({"path": "src/lib.rs", "old": "a - b", "new": "a + b"}),
            ),
            Phase::Planning,
            false,
        ));
        assert!(error && e.contains("plan"), "{e}");
        let (_, error) = output(tb.execute(
            &call(
                "edit_file",
                serde_json::json!({"path": "src/lib.rs", "old": "a - b", "new": "a + b"}),
            ),
            Phase::Working,
            false,
        ));
        assert!(!error);
        assert!(
            std::fs::read_to_string(root.join("src/lib.rs"))
                .unwrap()
                .contains("a + b")
        );
        let (e, error) = output(tb.execute(
            &call(
                "edit_file",
                serde_json::json!({"path": "src/lib.rs", "old": "missing", "new": "x"}),
            ),
            Phase::Working,
            false,
        ));
        assert!(error && e.contains("not in"), "{e}");
        let (_, error) = output(tb.execute(
            &call(
                "write_file",
                serde_json::json!({"path": "docs/a.md", "content": "# A\n"}),
            ),
            Phase::Working,
            false,
        ));
        assert!(!error && root.join("docs/a.md").is_file());
        let (_, error) = output(tb.execute(
            &call(
                "write_file",
                serde_json::json!({"path": ".env.local", "content": "X=1"}),
            ),
            Phase::Working,
            false,
        ));
        assert!(error && !root.join(".env.local").exists());
        match tb.execute(
            &call(
                "update_plan",
                serde_json::json!({"steps": [{"text": "Fix add"}, {"text": "Test", "done": true}]}),
            ),
            Phase::Planning,
            false,
        ) {
            Outcome::Plan(steps, _) => assert_eq!(
                steps[1],
                PlanStep {
                    text: "Test".into(),
                    done: true
                }
            ),
            other => panic!("{other:?}"),
        }
        let (e, error) = output(tb.execute(
            &call(
                "read_file",
                serde_json::json!({"__invalid": "arguments are not JSON"}),
            ),
            Phase::Working,
            false,
        ));
        assert!(error && e.contains("JSON"));
        assert_eq!(
            tb.execute(
                &call("finish", serde_json::json!({"summary": "Fixed."})),
                Phase::Working,
                false
            ),
            Outcome::Finished("Fixed.".into())
        );
    }

    #[test]
    fn commands_ask_without_a_sandbox_and_report_failures() {
        let root = repo("agent-run");
        let tb = ToolBox {
            root: root.clone(),
            sandbox: Sandbox::None,
        };
        let run = call("run", serde_json::json!({"command": "echo hi && exit 3"}));
        assert!(matches!(
            tb.execute(&run, Phase::Working, false),
            Outcome::NeedsApproval {
                access: Access::Sandboxed,
                ..
            }
        ));
        let (out, error) = output(tb.execute(&run, Phase::Working, true));
        assert!(
            error && out.starts_with("Exit code 3.") && out.contains("hi"),
            "{out}"
        );
        let slow = call(
            "run",
            serde_json::json!({"command": "sleep 5", "timeout_seconds": 1}),
        );
        let (out, error) = output(tb.execute(&slow, Phase::Working, true));
        assert!(error && out.contains("Stopped after 1 s"), "{out}");
        assert_eq!(clip(&"x".repeat(100)), "x".repeat(100));
        assert!(clip(&"y".repeat(20_000)).contains("bytes cut"));
    }

    /// A timeout stops what the command started as well, and nothing else.
    #[cfg(unix)]
    #[test]
    fn a_timeout_stops_the_commands_whole_group() {
        let root = repo("agent-timeout");
        let out = run_command(
            Sandbox::None,
            &root,
            "sleep 30 & echo $! > child.pid; wait",
            Access::Full,
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(out.timed_out);
        let pid = std::fs::read_to_string(root.join("child.pid")).unwrap();
        // A zombie counts as gone: it waits only for whoever reaps it.
        let alive = || {
            let out = Command::new("ps")
                .args(["-o", "stat=", "-p", pid.trim()])
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&out.stdout);
            !stat.trim().is_empty() && !stat.trim().starts_with('Z')
        };
        let start = Instant::now();
        while alive() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "the command's child survived the timeout"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_sandbox_keeps_commands_in_the_project() {
        let root = repo("agent-sandbox");
        let tb = ToolBox {
            root: root.clone(),
            sandbox: Sandbox::detect(),
        };
        assert_eq!(tb.sandbox, Sandbox::MacOs);
        let outside = dirs::home_dir()
            .unwrap()
            .join(format!("solder-agent-probe-{}", std::process::id()));
        let cmd = format!("echo in > inside.txt && echo out > '{}'", outside.display());
        let (out, error) = output(tb.execute(
            &call("run", serde_json::json!({"command": cmd})),
            Phase::Working,
            false,
        ));
        assert!(error, "{out}");
        assert!(root.join("inside.txt").is_file());
        assert!(!outside.exists());
        // Asking for the network waits for the user.
        let net = call(
            "run",
            serde_json::json!({"command": "curl -s https://example.com", "access": "network", "reason": "fetch"}),
        );
        assert!(matches!(
            tb.execute(&net, Phase::Working, false),
            Outcome::NeedsApproval {
                access: Access::Network,
                ..
            }
        ));
        let (out, error) = output(tb.execute(
            &call(
                "run",
                serde_json::json!({"command": "curl -s --max-time 5 https://example.com"}),
            ),
            Phase::Working,
            false,
        ));
        assert!(error, "the sandbox has no network: {out}");
    }
}
