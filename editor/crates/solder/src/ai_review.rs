//! What a push would send, for the AI review before it: the commits not on
//! the upstream yet and their diff, without files the project keeps from
//! AI. Blocks on `git`: call it from a background executor.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The diff sent to the model at most; bigger pushes are reviewed in part.
const DIFF_LIMIT: usize = 60_000;
const FILE_LIMIT: usize = 16_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Outgoing {
    /// What the commits are compared with: `origin/main`, or none for a
    /// branch with nothing to compare with.
    pub base: Option<String>,
    pub commits: Vec<String>,
    pub diff: String,
    /// Files left out: private to the project, or past the size limit.
    pub skipped: Vec<String>,
}

impl Outgoing {
    pub fn summary(&self) -> String {
        let commits: String = self.commits.iter().map(|c| format!("- {c}\n")).collect();
        let mut s = format!(
            "{} commit{} to push{}:\n{commits}",
            self.commits.len(),
            if self.commits.len() == 1 { "" } else { "s" },
            self.base
                .as_ref()
                .map(|b| format!(" onto {b}"))
                .unwrap_or_default()
        );
        if !self.skipped.is_empty() {
            s.push_str(&format!(
                "Left out of the diff: {}\n",
                self.skipped.join(", ")
            ));
        }
        s
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What `HEAD` is compared with: the upstream, else the remote's default
/// branch, else `main` or `master` when this is another branch.
fn base(root: &Path) -> Option<String> {
    let current = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let mut candidates = vec![];
    if let Some(up) = git(root, &["rev-parse", "--abbrev-ref", "@{upstream}"]) {
        candidates.push(up.trim().to_string());
    }
    if let Some(head) = git(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        candidates.push(head.trim().to_string());
    }
    candidates.extend(["origin/main", "origin/master", "main", "master"].map(String::from));
    candidates
        .into_iter()
        .filter(|c| *c != current)
        .find(|c| git(root, &["rev-parse", "--verify", "--quiet", c]).is_some())
}

/// The commits and diff a push of `HEAD` would send.
pub fn outgoing(root: &Path) -> Result<Outgoing, String> {
    git(root, &["rev-parse", "HEAD"]).ok_or("There are no commits to push")?;
    let base = base(root);
    let range = match &base {
        Some(b) => format!("{b}..HEAD"),
        None => "HEAD".into(),
    };
    let commits: Vec<String> = git(root, &["log", "--format=%h %s", "--max-count=50", &range])
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    if commits.is_empty() {
        return Ok(Outgoing {
            base,
            commits,
            diff: String::new(),
            skipped: Vec::new(),
        });
    }
    let diff = match &base {
        // Three dots: changes since the branches parted, not what the base
        // gained since.
        Some(b) => git(root, &["diff", "--no-color", "-U3", &format!("{b}...HEAD")]),
        None => git(root, &["show", "--no-color", "-U3", "--format=", "HEAD"]),
    }
    .ok_or("Could not read the diff")?;
    let (diff, skipped) = filter(root, &diff);
    Ok(Outgoing {
        base,
        commits,
        diff,
        skipped,
    })
}

/// The diff without files `.env` or `.solderignore` keep from AI, and
/// within the size limits.
fn filter(root: &Path, diff: &str) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut skipped = Vec::new();
    for file in split_files(diff) {
        let Some(path) = file_path(file) else {
            continue;
        };
        let full: PathBuf = root.join(&path);
        let allowed = crate::ai_context::allows_path(root, &full).unwrap_or(false)
            || !full.exists() && ai::context::allows_file(Path::new(&path));
        if !allowed {
            skipped.push(path);
            continue;
        }
        if file.len() > FILE_LIMIT || out.len() + file.len() > DIFF_LIMIT {
            skipped.push(path);
            continue;
        }
        out.push_str(file);
    }
    (out, skipped)
}

/// Splits a unified diff at its `diff --git` headers.
fn split_files(diff: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = diff
        .match_indices("diff --git ")
        .map(|(i, _)| i)
        .filter(|&i| i == 0 || diff.as_bytes()[i - 1] == b'\n')
        .collect();
    starts.push(diff.len());
    starts.windows(2).map(|w| &diff[w[0]..w[1]]).collect()
}

/// The new path of a file's diff (`+++ b/path`), or the old one if deleted.
fn file_path(file: &str) -> Option<String> {
    let new = file.lines().find_map(|l| l.strip_prefix("+++ b/"));
    let old = file.lines().find_map(|l| l.strip_prefix("--- a/"));
    new.or(old).map(str::to_string).or_else(|| {
        let header = file.lines().next()?;
        header.rsplit_once(" b/").map(|(_, p)| p.to_string())
    })
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
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        dir.canonicalize().unwrap()
    }

    #[test]
    fn collects_what_a_push_sends_without_private_files() {
        let root = repo("review-outgoing");
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&["switch", "-q", "-c", "feature"]);
        std::fs::write(root.join("a.rs"), "fn a() { let x = 1 / 0; }\n").unwrap();
        std::fs::write(root.join(".solderignore"), "secret/\n").unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(
            root.join("secret/keys.rs"),
            "const KEY: &str = \"sk-123\";\n",
        )
        .unwrap();
        std::fs::write(root.join(".env.example"), "TOKEN=abc\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "divide"]);
        let out = outgoing(&root).unwrap();
        assert_eq!(out.base.as_deref(), Some("main"));
        assert_eq!(out.commits.len(), 1);
        assert!(out.commits[0].ends_with("divide"));
        assert!(out.diff.contains("1 / 0"), "{}", out.diff);
        assert!(!out.diff.contains("sk-123") && !out.diff.contains("TOKEN"));
        assert!(out.skipped.contains(&"secret/keys.rs".to_string()));
        assert!(out.skipped.contains(&".env.example".to_string()));
        assert!(out.summary().starts_with("1 commit to push onto main:"));

        // Nothing new on main itself compared with... nothing to compare.
        run(&["switch", "-q", "main"]);
        let out = outgoing(&root).unwrap();
        assert_eq!(out.base, None);
        assert_eq!(out.commits.len(), 1);
        assert!(out.diff.contains("fn a()"));
    }

    #[test]
    fn splits_diffs_by_file() {
        let diff = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ /dev/null\n";
        let files = split_files(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(file_path(files[0]).as_deref(), Some("x.rs"));
        assert_eq!(file_path(files[1]).as_deref(), Some("gone.rs"));
    }
}
