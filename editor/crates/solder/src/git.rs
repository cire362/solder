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

impl Repo {
    /// The repository containing `path`, if any.
    pub fn discover(path: &Path) -> Option<Repo> {
        let out = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--show-toplevel"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status.success().then(|| Repo {
            workdir: PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()),
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.workdir)
            // Never wait for an editor or a credential prompt.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .args(["-c", "core.quotepath=off"])
            .args(args);
        cmd
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.run_with_input(args, None)
    }

    fn run_with_input(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut child = self
            .command(args)
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
                if kind == "2" {
                    // The original path follows as its own record.
                    records.next();
                }
                let conflicted = kind == "u";
                status.files.push(FileStatus {
                    path,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(s.files[3].conflicted);
        assert!(s.files[4].untracked);
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
        let dir = std::env::temp_dir().join(format!("solder-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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
