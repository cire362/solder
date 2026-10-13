//! Git through the `git` command line, the way most editors do it: nothing to
//! link, the user's own config and hooks apply, and the output formats used
//! here (`--porcelain=v2 -z`) are stable by contract.
//!
//! Everything here blocks; callers run it on the background executor.

use std::{
    io::Write,
    ops::Range,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use text::diff::{Hunk, diff_lines, lines};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    Renamed,
    TypeChanged,
}

impl Change {
    fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            b'A' => Change::Added,
            b'M' => Change::Modified,
            b'D' => Change::Deleted,
            b'R' | b'C' => Change::Renamed,
            b'T' => Change::TypeChanged,
            _ => return None,
        })
    }

    pub fn letter(self) -> &'static str {
        match self {
            Change::Added => "A",
            Change::Modified => "M",
            Change::Deleted => "D",
            Change::Renamed => "R",
            Change::TypeChanged => "T",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    /// Relative to the repository root, `/`-separated.
    pub path: String,
    pub original_path: Option<String>,
    pub staged: Option<Change>,
    pub unstaged: Option<Change>,
    pub untracked: bool,
    pub conflicted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoStatus {
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<FileStatus>,
}

#[derive(Clone, Debug)]
pub struct GitError(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffScope {
    Working,
    Staged,
}

pub struct FileDiffSnapshot {
    pub path: String,
    pub old: String,
    pub new: String,
    pub can_open: bool,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, GitError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    pub workdir: PathBuf,
}

/// Build at the checkout, so the transport sets cwd and environment on
/// the target rather than passing a virtual cache path to `git -C`.
pub(crate) fn command(dir: &Path, args: &[&str]) -> std::io::Result<Command> {
    command_with(dir, args, crate::remote::command)
}

fn command_with(
    dir: &Path,
    args: &[&str],
    launch: impl FnOnce(&Path, &str, &[String], &[(String, String)]) -> std::io::Result<Command>,
) -> std::io::Result<Command> {
    let mut argv = vec!["-c".into(), "core.quotepath=off".into()];
    argv.extend(args.iter().map(|arg| (*arg).to_string()));
    launch(
        dir,
        "git",
        &argv,
        &[
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_EDITOR".into(), "true".into()),
        ],
    )
}

/// Only outputs known to be absolute paths pass through this mapping.
pub(crate) fn workdir(dir: &Path, output: &str) -> Option<PathBuf> {
    workdir_with(
        dir,
        output,
        crate::remote::is_remote(dir),
        crate::remote::local_path,
    )
}

fn workdir_with(
    dir: &Path,
    output: &str,
    remote: bool,
    local: impl FnOnce(&Path, &str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    if remote {
        // Git adds one newline. Spaces and newlines in the path stay raw.
        local(dir, output.strip_suffix('\n').unwrap_or(output))
    } else {
        Some(PathBuf::from(output.trim()))
    }
}

pub(crate) fn command_path(dir: &Path, path: &Path) -> std::io::Result<String> {
    command_path_with(path, crate::remote::is_remote(dir), |path| {
        crate::remote::remote_path(path)
            .filter(|target| crate::remote::local_path(dir, target).as_deref() == Some(path))
    })
}

fn command_path_with(
    path: &Path,
    remote: bool,
    target: impl FnOnce(&Path) -> Option<String>,
) -> std::io::Result<String> {
    if remote {
        return target(path).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                crate::lang::t("Cannot map the remote repository path."),
            )
        });
    }
    Ok(path.to_string_lossy().into_owned())
}

impl Repo {
    /// The repository containing `path`, if any.
    pub fn discover(path: &Path) -> Option<Repo> {
        let out = command(path, &["rev-parse", "--show-toplevel"])
            .ok()?
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(Repo {
            workdir: workdir(path, &String::from_utf8_lossy(&out.stdout))?,
        })
    }

    pub(crate) fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.run_with_input(args, None)
    }

    pub(crate) fn run_with_input(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut child = command(&self.workdir, args)
            .map_err(|e| GitError(format!("git: {e}")))?
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| GitError(format!("git: {e}")))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin
                .write_all(input)
                .map_err(|e| GitError(e.to_string()))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| GitError(e.to_string()))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let message = stderr
                .lines()
                .map(|l| {
                    l.trim_start_matches("error: ")
                        .trim_start_matches("fatal: ")
                })
                .find(|l| !l.trim().is_empty())
                .unwrap_or("git failed")
                .to_string();
            Err(GitError(message))
        }
    }

    pub fn relative(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.workdir).ok()?;
        Some(rel.to_string_lossy().replace('\\', "/"))
    }

    pub fn status(&self) -> Result<RepoStatus> {
        let out = self.run(&[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
        ])?;
        Ok(parse_status(&out))
    }

    /// `git show <spec>`: `:path` is the index, `HEAD:path` the last commit,
    /// `:1:path`, `:2:path`, `:3:path` the base, ours and theirs of a conflict.
    pub fn show(&self, spec: &str) -> Option<String> {
        self.run(&["show", spec])
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    pub fn index_text(&self, rel: &str) -> Option<String> {
        self.show(&format!(":{rel}"))
    }

    /// Both sides of a file comparison. `status` is the repository status
    /// the caller already has; pass `None` to run `git status` here. A full
    /// status walks the whole working tree, which is slow in a large
    /// repository, so the workspace passes its cached copy.
    pub fn file_diff(
        &self,
        rel: &str,
        scope: DiffScope,
        working_text: Option<String>,
        status: Option<&RepoStatus>,
    ) -> Result<FileDiffSnapshot> {
        let fetched;
        let status = match status {
            Some(status) => status,
            None => {
                fetched = self.status()?;
                &fetched
            }
        };
        let file = status.files.iter().find(|file| file.path == rel);
        if file.is_some_and(|file| file.conflicted) {
            return Err(GitError("Resolve this file in the conflict view.".into()));
        }
        let decode = |bytes: Vec<u8>| {
            if bytes.contains(&0) {
                return Err(GitError("Binary files cannot be compared as text.".into()));
            }
            String::from_utf8(bytes).map_err(|_| GitError("This file is not UTF-8 text.".into()))
        };
        let index = || {
            if file.is_some_and(|file| file.untracked || file.staged == Some(Change::Deleted)) {
                Ok(String::new())
            } else {
                decode(self.run(&["show", &format!(":0:{rel}")])?)
            }
        };
        let (old, new) = match scope {
            DiffScope::Staged => {
                let old = if file
                    .is_some_and(|file| file.untracked || file.staged == Some(Change::Added))
                {
                    String::new()
                } else {
                    let original = file
                        .filter(|file| file.staged == Some(Change::Renamed))
                        .and_then(|file| file.original_path.as_deref())
                        .unwrap_or(rel);
                    decode(self.run(&["show", &format!("HEAD:{original}")])?)?
                };
                (old, index()?)
            }
            DiffScope::Working => {
                let new = match working_text {
                    Some(text) => decode(text.into_bytes())?,
                    None => match crate::remote::read(&self.workdir.join(rel)) {
                        Ok(bytes) => decode(bytes)?,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::NotFound
                                && file
                                    .is_some_and(|file| file.unstaged == Some(Change::Deleted)) =>
                        {
                            String::new()
                        }
                        Err(error) => return Err(GitError(format!("Cannot read {rel}: {error}"))),
                    },
                };
                (index()?, new)
            }
        };
        Ok(FileDiffSnapshot {
            path: rel.to_string(),
            old,
            new,
            can_open: if crate::remote::is_remote(&self.workdir) {
                crate::remote::metadata(&self.workdir.join(rel))
                    .is_ok_and(|metadata| metadata.is_file() && !metadata.is_symlink())
            } else {
                self.workdir.join(rel).is_file()
            },
        })
    }

    pub fn stage(&self, paths: &[&str]) -> Result<()> {
        let mut args = vec!["add", "--"];
        args.extend(paths);
        self.run(&args).map(drop)
    }

    pub fn unstage(&self, paths: &[&str]) -> Result<()> {
        let mut args = vec!["reset", "-q", "--"];
        args.extend(paths);
        match self.run(&args) {
            Ok(_) => Ok(()),
            // No commits yet: nothing to reset to, so drop from the index.
            Err(_) => {
                let mut args = vec!["rm", "--cached", "-q", "--"];
                args.extend(paths);
                self.run(&args).map(drop)
            }
        }
    }

    /// Throws away unstaged changes to tracked files.
    pub fn discard(&self, paths: &[&str]) -> Result<()> {
        let mut args = vec!["checkout", "-q", "--"];
        args.extend(paths);
        self.run(&args).map(drop)
    }

    /// Writes `content` as the staged version of `rel`, leaving the working
    /// file alone. This is how single lines get staged.
    pub fn stage_content(&self, rel: &str, content: &str) -> Result<()> {
        let blob = self.run_with_input(
            &["hash-object", "-w", "--stdin", "--path", rel],
            Some(content.as_bytes()),
        )?;
        let blob = String::from_utf8_lossy(&blob).trim().to_string();
        let mode = self
            .run(&["ls-files", "-s", "--", rel])
            .ok()
            .and_then(|out| {
                String::from_utf8_lossy(&out)
                    .split_whitespace()
                    .next()
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "100644".into());
        self.run(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("{mode},{blob},{rel}"),
        ])
        .map(drop)
    }

    /// Commits the index. Returns the first line of git's summary.
    pub fn commit(&self, message: &str, amend: bool) -> Result<String> {
        self.check_staged_secrets()?;
        let mut args = vec!["commit", "-F", "-"];
        if amend {
            args.push("--amend");
        }
        let out = self.run_with_input(&args, Some(message.as_bytes()))?;
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string())
    }

    pub fn branches(&self) -> Result<Vec<String>> {
        let out = self.run(&[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/heads",
        ])?;
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_owned)
            .collect())
    }

    pub fn switch(&self, branch: &str, create: bool) -> Result<()> {
        if create {
            self.run(&["switch", "-c", branch]).map(drop)
        } else {
            self.run(&["switch", branch]).map(drop)
        }
    }

    pub fn remote_url(&self) -> Option<String> {
        self.run(&["remote", "get-url", "origin"])
            .ok()
            .map(|b| String::from_utf8_lossy(&b).trim().to_string())
    }
}

pub fn parse_status(out: &[u8]) -> RepoStatus {
    let mut status = RepoStatus::default();
    let mut records = out.split(|b| *b == 0).filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        let text = String::from_utf8_lossy(record);
        let mut fields = text.splitn(2, ' ');
        let kind = fields.next().unwrap_or_default();
        let rest = fields.next().unwrap_or_default();
        match kind {
            "#" => {
                if let Some(head) = rest.strip_prefix("branch.head ") {
                    status.branch = (head != "(detached)").then(|| head.to_string());
                } else if let Some(up) = rest.strip_prefix("branch.upstream ") {
                    status.upstream = Some(up.to_string());
                } else if let Some(ab) = rest.strip_prefix("branch.ab ") {
                    let mut parts = ab.split_whitespace();
                    status.ahead = parts
                        .next()
                        .and_then(|a| a.trim_start_matches('+').parse().ok())
                        .unwrap_or(0);
                    status.behind = parts
                        .next()
                        .and_then(|b| b.trim_start_matches('-').parse().ok())
                        .unwrap_or(0);
                }
            }
            "1" | "2" | "u" => {
                // Path is the last field: 7 more fields for "1", 8 for "2"
                // (plus score), 9 for "u".
                let skip = match kind {
                    "1" => 7,
                    "2" => 8,
                    _ => 9,
                };
                let mut parts = rest.splitn(skip + 1, ' ');
                let xy = parts.next().unwrap_or("..").as_bytes().to_vec();
                let path = parts.nth(skip - 1).unwrap_or_default().to_string();
                let original_path = (kind == "2")
                    .then(|| {
                        records
                            .next()
                            .map(|path| String::from_utf8_lossy(path).into_owned())
                    })
                    .flatten();
                let conflicted = kind == "u";
                status.files.push(FileStatus {
                    path,
                    original_path,
                    staged: if conflicted {
                        None
                    } else {
                        Change::from_code(xy[0])
                    },
                    unstaged: if conflicted {
                        None
                    } else {
                        Change::from_code(xy[1])
                    },
                    untracked: false,
                    conflicted,
                });
            }
            "?" => status.files.push(FileStatus {
                path: rest.to_string(),
                original_path: None,
                staged: None,
                unstaged: None,
                untracked: true,
                conflicted: false,
            }),
            _ => {}
        }
    }
    status
}

/// Whether a selection of rows touches a hunk. A deletion has no rows of its
/// own, so it counts when the selection reaches the row where it happened.
pub fn hunk_touched(hunk: &Hunk, rows: &Range<usize>) -> bool {
    if hunk.is_deletion() {
        rows.start <= hunk.new.start && hunk.new.start <= rows.end.max(rows.start + 1)
    } else {
        hunk.new.start < rows.end.max(rows.start + 1) && rows.start < hunk.new.end
    }
}

/// The index content after staging `rows` of `current` on top of `base`.
///
/// Added lines stage one by one: only the selected ones go in. A hunk that
/// removes or rewrites lines stages as a whole when the selection touches
/// it, because half a replacement rarely means anything.
pub fn stage_rows(base: &str, current: &str, rows: Range<usize>) -> String {
    let old = lines(base);
    let new = lines(current);
    let mut out: Vec<&str> = Vec::with_capacity(old.len().max(new.len()));
    let mut at = 0;
    for hunk in diff_lines(&old, &new) {
        out.extend_from_slice(&old[at..hunk.old.start]);
        if !hunk_touched(&hunk, &rows) {
            out.extend_from_slice(&old[hunk.old.clone()]);
        } else if hunk.is_insertion() {
            let from = hunk.new.start.max(rows.start);
            let to = hunk.new.end.min(rows.end.max(rows.start + 1));
            out.extend_from_slice(&new[from..to.max(from)]);
        } else {
            out.extend_from_slice(&new[hunk.new.clone()]);
        }
        at = hunk.old.end;
    }
    out.extend_from_slice(&old[at..]);
    out.join("\n")
}

/// Web URL for opening a pull request from `branch`, for GitHub and GitLab
/// remotes in either SSH or HTTPS form.
pub fn pull_request_url(remote: &str, branch: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches(".git");
    let (host, repo) = if let Some(rest) = remote.strip_prefix("git@") {
        rest.split_once(':')?
    } else {
        let rest = remote
            .strip_prefix("https://")
            .or_else(|| remote.strip_prefix("http://"))?;
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        rest.split_once('/')?
    };
    match host {
        h if h.contains("gitlab") => Some(format!(
            "https://{host}/{repo}/-/merge_requests/new?merge_request[source_branch]={branch}"
        )),
        _ => Some(format!("https://{host}/{repo}/compare/{branch}?expand=1")),
    }
}

/// Whether what was typed is the address of a repository and not the
/// name of a project: something `git clone` would be given.
pub fn is_address(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return false;
    }
    // `git@host:owner/name`, the short way to write an SSH address.
    let short = text.split_once(':').is_some_and(|(host, path)| {
        host.contains('@') && !host.contains('/') && !path.is_empty() && !path.starts_with("//")
    });
    text.contains("://") || short || text.ends_with(".git")
}

/// The folder a clone of `address` gets: `solder` for
/// `https://example.com/cire362/solder.git`.
pub fn clone_name(address: &str) -> Option<String> {
    let address = address.trim().trim_end_matches('/');
    let address = address.strip_suffix(".git").unwrap_or(address);
    let name = address.rsplit(['/', ':', '\\']).next()?;
    (!name.is_empty() && name != "." && name != ".." && !name.contains('@'))
        .then(|| name.to_string())
}

/// Clones `address` into `into`, saying what git says of how far it is.
/// Stopped through `stop`, it ends git and leaves what git leaves.
pub fn clone(
    address: &str,
    into: &Path,
    stop: &std::sync::atomic::AtomicBool,
    mut said: impl FnMut(String),
) -> Result<()> {
    use std::io::Read;
    use std::sync::atomic::Ordering;
    let remote = crate::remote::is_remote(into);
    let cwd = if remote {
        into.parent()
            .ok_or_else(|| {
                GitError(crate::lang::t("Cannot map the remote repository path.").into())
            })?
            .to_path_buf()
    } else {
        std::env::current_dir().map_err(|e| GitError(format!("git: {e}")))?
    };
    if remote && !crate::remote::is_remote(&cwd) {
        return Err(GitError(
            crate::lang::t("Cannot map the remote repository path.").into(),
        ));
    }
    let mut args = vec![
        "clone".into(),
        "--progress".into(),
        "--".into(),
        address.into(),
    ];
    if remote {
        args.push(command_path(&cwd, into).map_err(|e| GitError(format!("git: {e}")))?);
    }
    let mut cmd = crate::remote::command(
        &cwd,
        "git",
        &args,
        &[("GIT_TERMINAL_PROMPT".into(), "0".into())],
    )
    .map_err(|e| GitError(format!("git: {e}")))?;
    if !remote {
        // Keep native, possibly non-UTF-8, local destination paths.
        cmd.arg(into);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| GitError(format!("git: {e}")))?;
    // Progress is one line written over and over: a part ends at either
    // kind of line end.
    let (send, lines) = std::sync::mpsc::channel::<String>();
    let reader = child.stderr.take().map(|mut errors| {
        std::thread::spawn(move || {
            let (mut chunk, mut line) = ([0u8; 4096], Vec::new());
            while let Ok(read @ 1..) = errors.read(&mut chunk) {
                for byte in &chunk[..read] {
                    if !matches!(byte, b'\r' | b'\n') {
                        line.push(*byte);
                        continue;
                    }
                    let text = String::from_utf8_lossy(&line).trim().to_string();
                    line.clear();
                    if !text.is_empty() && send.send(text).is_err() {
                        return;
                    }
                }
            }
            let text = String::from_utf8_lossy(&line).trim().to_string();
            if !text.is_empty() {
                let _ = send.send(text);
            }
        })
    });
    let mut reason = None;
    let mut hear = |line: String, said: &mut dyn FnMut(String)| {
        let cause = line
            .strip_prefix("fatal: ")
            .or_else(|| line.strip_prefix("error: "));
        match cause {
            // The first is the cause; what follows is what came of it.
            Some(cause) => drop(reason.get_or_insert_with(|| cause.to_string())),
            None => said(line),
        }
    };
    let status = loop {
        for line in lines.try_iter() {
            hear(line, &mut said);
        }
        if stop.load(Ordering::Relaxed) {
            // Asked to end, git takes away what it had made so far.
            #[cfg(unix)]
            let _ = Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            #[cfg(not(unix))]
            let _ = child.kill();
            let _ = child.wait();
            return Err(GitError("Stopped".into()));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(30)),
            Err(e) => return Err(GitError(e.to_string())),
        }
    };
    if let Some(reader) = reader {
        let _ = reader.join();
    }
    for line in lines.try_iter() {
        hear(line, &mut said);
    }
    match status.success() {
        true => Ok(()),
        false => Err(GitError(reason.unwrap_or_else(|| "git failed".into()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_path_arguments_are_explicit_and_never_fall_back_to_cache() {
        let path = Path::new("/virtual/cache/repo/.solder/worktrees/a task");
        let target = "/srv/repo/.solder/worktrees/a task";
        assert_eq!(
            command_path_with(path, true, |value| {
                assert_eq!(value, path);
                Some(target.into())
            })
            .unwrap(),
            target
        );
        assert!(command_path_with(path, true, |_| None).is_err());
        assert_eq!(
            command_path_with(path, false, |_| panic!("local path mapping")).unwrap(),
            path.to_string_lossy()
        );
    }

    #[test]
    fn routing_keeps_commit_text_and_file_arguments_raw() {
        let cwd = Path::new("/virtual/cache/repo");
        let message = "Keep /virtual/cache/repo and $(literal)\nSecond line 'quoted'";
        let built = command_with(
            cwd,
            &["commit", "-m", message],
            |dir, program, args, env| {
                assert_eq!(dir, cwd);
                assert_eq!(program, "git");
                assert_eq!(args, &["-c", "core.quotepath=off", "commit", "-m", message]);
                assert_eq!(
                    env,
                    &[
                        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                        ("GIT_EDITOR".into(), "true".into())
                    ]
                );
                let mut fake = Command::new("fake-ssh");
                fake.args(args);
                Ok(fake)
            },
        )
        .unwrap();
        assert_eq!(built.get_program(), "fake-ssh");
        assert_eq!(built.get_args().last().unwrap(), message);
        command_with(
            cwd,
            &["show", "HEAD:literal /virtual/cache/repo"],
            |_, _, args, _| {
                assert_eq!(args.last().unwrap(), "HEAD:literal /virtual/cache/repo");
                Ok(Command::new("fake-docker"))
            },
        )
        .unwrap();
        assert!(
            command_with(cwd, &["status"], |_, _, _, _| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "closed mount",
                ))
            })
            .is_err()
        );
    }

    #[test]
    fn discovery_maps_only_the_root_and_preserves_path_whitespace() {
        let cache = Path::new("/virtual/cache/repo");
        let target = "/srv/a repo \n";
        let output = format!("{target}\n");
        assert_eq!(
            workdir_with(cache, &output, true, |root, value| {
                assert_eq!(root, cache);
                assert_eq!(value, target);
                Some(cache.to_path_buf())
            })
            .as_deref(),
            Some(cache)
        );
        assert!(workdir_with(cache, "/outside\n", true, |_, _| None).is_none());
        assert_eq!(
            workdir_with(cache, " /local/repo\n", false, |_, _| panic!(
                "local path must stay local"
            ))
            .unwrap(),
            Path::new("/local/repo")
        );
    }

    #[test]
    fn an_address_is_told_from_a_name() {
        for address in [
            "https://example.com/cire362/solder.git",
            "https://example.com/cire362/solder",
            "git@example.com:cire362/solder.git",
            "ssh://git@example.com/cire362/solder",
            "file:///tmp/origin",
            "../origin.git",
        ] {
            assert!(is_address(address), "{address}");
        }
        for name in ["solder", "my project", "", "src/main.rs", "a:b", "C:/code"] {
            assert!(!is_address(name), "{name}");
        }
        let name = |address| clone_name(address).unwrap();
        assert_eq!(name("https://example.com/cire362/solder.git"), "solder");
        assert_eq!(name("https://example.com/cire362/solder/"), "solder");
        assert_eq!(name("git@example.com:solder.git"), "solder");
        assert_eq!(name("file:///tmp/origin"), "origin");
        assert_eq!(clone_name("https://example.com/.."), None);
        assert_eq!(clone_name("git@example.com"), None);
    }

    #[test]
    fn a_clone_brings_the_files_and_says_why_it_could_not() {
        let origin = diff_repo("clone-origin");
        std::fs::write(origin.workdir.join("README.md"), "hello\n").unwrap();
        origin.run(&["add", "."]).unwrap();
        origin.run(&["commit", "-q", "-m", "first"]).unwrap();
        let home = db::testing::dir("clone-into");
        let stop = std::sync::atomic::AtomicBool::new(false);
        let address = format!("file://{}", origin.workdir.display());
        clone(&address, &home.join("copy"), &stop, |_| {}).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("copy/README.md")).unwrap(),
            "hello\n"
        );
        // The same place again: git says why not, and that is the error.
        let again = clone(&address, &home.join("copy"), &stop, |_| {}).unwrap_err();
        assert!(again.0.contains("already exists"), "{}", again.0);
        let missing = format!("file://{}", home.join("nothing").display());
        let none = clone(&missing, &home.join("other"), &stop, |_| {}).unwrap_err();
        assert!(none.0.contains("nothing"), "{}", none.0);
        assert!(!home.join("other").exists());
        // Asked to stop before it began, it makes nothing.
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let stopped = clone(&address, &home.join("third"), &stop, |_| {}).unwrap_err();
        assert_eq!(stopped.0, "Stopped");
    }

    #[test]
    fn parses_porcelain_v2() {
        let out = b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 aaa bbb src/a.rs\0\
1 A. N... 000000 100644 100644 000 ccc new file.rs\0\
2 R. N... 100644 100644 100644 ddd eee R100 moved.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 f1 f2 f3 both.rs\0\
? scratch.txt\0";
        let s = parse_status(out);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.ahead, s.behind), (2, 1));
        let paths: Vec<&str> = s.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "src/a.rs",
                "new file.rs",
                "moved.rs",
                "both.rs",
                "scratch.txt"
            ]
        );
        assert_eq!(s.files[0].unstaged, Some(Change::Modified));
        assert_eq!(s.files[0].staged, None);
        assert_eq!(s.files[1].staged, Some(Change::Added));
        assert_eq!(s.files[2].staged, Some(Change::Renamed));
        assert_eq!(s.files[2].original_path.as_deref(), Some("old.rs"));
        assert!(s.files[3].conflicted);
        assert!(s.files[4].untracked);
    }

    fn diff_repo(name: &str) -> Repo {
        let dir = db::testing::dir(&format!("diff-{name}"));
        let repo = Repo { workdir: dir };
        repo.run(&["init", "-q", "-b", "main"]).unwrap();
        repo.run(&["config", "user.name", "Test"]).unwrap();
        repo.run(&["config", "user.email", "test@example.com"])
            .unwrap();
        repo
    }

    #[test]
    fn diff_compares_head_index_and_working_text_independently() {
        let repo = diff_repo("versions");
        let path = repo.workdir.join("file with spaces π.txt");
        let rel = repo.relative(&path).unwrap();
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let untracked = repo
            .file_diff(&rel, DiffScope::Working, None, None)
            .unwrap();
        assert_eq!(
            (untracked.old.as_str(), untracked.new.as_str()),
            ("", "one\ntwo\n")
        );
        repo.stage(&[&rel]).unwrap();
        let first = repo.file_diff(&rel, DiffScope::Staged, None, None).unwrap();
        assert_eq!((first.old.as_str(), first.new.as_str()), ("", "one\ntwo\n"));
        repo.commit("initial", false).unwrap();
        std::fs::write(&path, "one\nstaged\n").unwrap();
        repo.stage(&[&rel]).unwrap();
        std::fs::write(&path, "one\nworking\n").unwrap();
        let working = repo
            .file_diff(&rel, DiffScope::Working, None, None)
            .unwrap();
        assert_eq!(
            (working.old.as_str(), working.new.as_str()),
            ("one\nstaged\n", "one\nworking\n")
        );
        let buffer = repo
            .file_diff(&rel, DiffScope::Working, Some("unsaved\n".into()), None)
            .unwrap();
        assert_eq!(buffer.new, "unsaved\n");
        let staged = repo
            .file_diff(&rel, DiffScope::Staged, Some("unsaved\n".into()), None)
            .unwrap();
        assert_eq!(
            (staged.old.as_str(), staged.new.as_str()),
            ("one\ntwo\n", "one\nstaged\n")
        );
        repo.commit("staged", false).unwrap();
        repo.discard(&[&rel]).unwrap();
        repo.run(&["mv", "--", &rel, "renamed.txt"]).unwrap();
        let renamed = repo
            .file_diff("renamed.txt", DiffScope::Staged, None, None)
            .unwrap();
        assert_eq!(renamed.old, "one\nstaged\n");
        assert_eq!(renamed.old, renamed.new);
        repo.commit("rename", false).unwrap();
        std::fs::remove_file(repo.workdir.join("renamed.txt")).unwrap();
        let deleted = repo
            .file_diff("renamed.txt", DiffScope::Working, None, None)
            .unwrap();
        assert_eq!(deleted.old, "one\nstaged\n");
        assert!(deleted.new.is_empty());
        repo.stage(&["renamed.txt"]).unwrap();
        let deleted = repo
            .file_diff("renamed.txt", DiffScope::Staged, None, None)
            .unwrap();
        assert_eq!(deleted.old, "one\nstaged\n");
        assert!(deleted.new.is_empty());
        std::fs::remove_dir_all(repo.workdir).unwrap();
    }

    #[test]
    fn diff_uses_the_status_it_is_given() {
        let repo = diff_repo("cached-status");
        std::fs::write(repo.workdir.join("a.txt"), "one\n").unwrap();
        repo.stage(&["a.txt"]).unwrap();
        repo.commit("initial", false).unwrap();
        std::fs::write(repo.workdir.join("a.txt"), "two\n").unwrap();
        // A status that calls the tracked file untracked: the index side comes
        // out empty, which proves git status did not run again.
        let cached = RepoStatus {
            files: vec![FileStatus {
                path: "a.txt".into(),
                original_path: None,
                staged: None,
                unstaged: None,
                untracked: true,
                conflicted: false,
            }],
            ..Default::default()
        };
        let diff = repo
            .file_diff("a.txt", DiffScope::Working, None, Some(&cached))
            .unwrap();
        assert_eq!((diff.old.as_str(), diff.new.as_str()), ("", "two\n"));
        let fresh = repo
            .file_diff("a.txt", DiffScope::Working, None, None)
            .unwrap();
        assert_eq!(fresh.old, "one\n");
        std::fs::remove_dir_all(repo.workdir).unwrap();
    }

    #[test]
    fn diff_rejects_binary_non_utf8_and_missing_files() {
        let repo = diff_repo("errors");
        std::fs::write(repo.workdir.join("binary.txt"), b"text\0data").unwrap();
        assert!(
            repo.file_diff("binary.txt", DiffScope::Working, None, None)
                .err()
                .unwrap()
                .0
                .contains("Binary")
        );
        std::fs::write(repo.workdir.join("encoding.txt"), [0xff]).unwrap();
        assert!(
            repo.file_diff("encoding.txt", DiffScope::Working, None, None)
                .err()
                .unwrap()
                .0
                .contains("UTF-8")
        );
        assert!(
            repo.file_diff("absent.txt", DiffScope::Working, None, None)
                .err()
                .unwrap()
                .0
                .contains("Cannot read")
        );
        std::fs::remove_dir_all(repo.workdir).unwrap();
    }

    #[test]
    fn stages_selected_added_lines_only() {
        let base = "a\nb\n";
        let current = "a\nx\ny\nb\n";
        // Stage only "y" (row 2).
        assert_eq!(stage_rows(base, current, 2..3), "a\ny\nb\n");
        // Selection elsewhere changes nothing.
        assert_eq!(stage_rows(base, current, 0..1), base);
        // A rewrite stages whole.
        assert_eq!(stage_rows("a\nb\nc", "a\nB\nc", 1..2), "a\nB\nc");
        // A deletion stages from the row where it happened.
        assert_eq!(stage_rows("a\nb\nc", "a\nc", 1..1), "a\nc");
    }

    #[test]
    fn pull_request_urls() {
        assert_eq!(
            pull_request_url("git@github.com:cire362/solder.git", "feat/git").unwrap(),
            "https://github.com/cire362/solder/compare/feat/git?expand=1"
        );
        assert_eq!(
            pull_request_url("https://github.com/cire362/solder.git", "x").unwrap(),
            "https://github.com/cire362/solder/compare/x?expand=1"
        );
        assert!(
            pull_request_url("https://gitlab.com/a/b", "x")
                .unwrap()
                .contains("merge_requests/new")
        );
    }

    /// A throwaway repository driven through the real `git` binary.
    #[test]
    fn stage_commit_and_status_round_trip() {
        let dir = db::testing::dir("git");
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                ok.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&ok.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        let repo = Repo::discover(&dir).unwrap();
        assert!(repo.status().unwrap().files[0].untracked);

        repo.stage(&["a.txt"]).unwrap();
        assert_eq!(repo.status().unwrap().files[0].staged, Some(Change::Added));
        repo.commit("first", false).unwrap();
        assert!(repo.status().unwrap().files.is_empty());

        // Stage one of two new lines through the index directly.
        std::fs::write(dir.join("a.txt"), "one\nnew1\nnew2\ntwo\n").unwrap();
        let base = repo.index_text("a.txt").unwrap();
        let staged = stage_rows(&base, "one\nnew1\nnew2\ntwo\n", 1..2);
        repo.stage_content("a.txt", &staged).unwrap();
        assert_eq!(repo.index_text("a.txt").unwrap(), "one\nnew1\ntwo\n");
        let file = &repo.status().unwrap().files[0];
        assert_eq!(
            (file.staged, file.unstaged),
            (Some(Change::Modified), Some(Change::Modified))
        );

        repo.unstage(&["a.txt"]).unwrap();
        assert_eq!(repo.index_text("a.txt").unwrap(), "one\ntwo\n");
        repo.discard(&["a.txt"]).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "one\ntwo\n"
        );

        repo.switch("feat/x", true).unwrap();
        assert_eq!(repo.status().unwrap().branch.as_deref(), Some("feat/x"));
        assert!(repo.branches().unwrap().contains(&"main".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
