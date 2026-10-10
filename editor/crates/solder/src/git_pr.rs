//! A single snapshot from GitHub CLI. Refresh is explicit; no watcher runs
//! in the background and no credentials are copied into the editor.

use crate::git::{GitError, Repo, Result};
use std::{
    ffi::OsStr,
    path::Path,
    process::{Command, Stdio},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Passed,
    Failed,
    Pending,
    Skipped,
    Cancelled,
}
impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Passed => "Passed",
            Self::Failed => "Failed",
            Self::Pending => "Pending",
            Self::Skipped => "Skipped",
            Self::Cancelled => "Cancelled",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub state: State,
    pub url: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub draft: bool,
    pub head: String,
    pub base: String,
    pub checks: Vec<Check>,
}

pub fn read(repo: &Repo, branch: &str, program: &Path) -> Result<PullRequest> {
    let out = Command::new(program)
        .current_dir(&repo.workdir)
        .args([
            OsStr::new("pr"),
            OsStr::new("view"),
            OsStr::new(branch),
            OsStr::new("--json"),
            OsStr::new("number,title,url,state,isDraft,headRefName,baseRefName,statusCheckRollup"),
        ])
        .env("GH_PROMPT_DISABLED", "true")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| {
            GitError(if e.kind() == std::io::ErrorKind::NotFound {
                "Install GitHub CLI (gh) to read pull requests.".into()
            } else {
                format!("GitHub CLI: {e}")
            })
        })?;
    if !out.status.success() {
        let message = String::from_utf8_lossy(&out.stderr)
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Could not read the branch's pull request.")
            .to_string();
        return Err(GitError(message));
    }
    parse(&out.stdout)
}

fn link(value: &serde_json::Value) -> Option<String> {
    let s = value.as_str()?;
    (s.starts_with("https://") || s.starts_with("http://")).then(|| s.into())
}
fn parse(bytes: &[u8]) -> Result<PullRequest> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| GitError("GitHub CLI returned unreadable pull request data.".into()))?;
    let required = |field: &str| {
        value[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| GitError(format!("GitHub CLI did not return {field}.")))
    };
    let mut checks = Vec::new();
    for check in value["statusCheckRollup"].as_array().into_iter().flatten() {
        let result = check["conclusion"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| check["state"].as_str())
            .unwrap_or("");
        let state = match result {
            "SUCCESS" | "NEUTRAL" => State::Passed,
            "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "STALE" => {
                State::Failed
            }
            "SKIPPED" => State::Skipped,
            "CANCELLED" => State::Cancelled,
            _ => State::Pending,
        };
        checks.push(Check {
            name: check["name"]
                .as_str()
                .or_else(|| check["context"].as_str())
                .unwrap_or("Check")
                .into(),
            state,
            url: link(&check["detailsUrl"]).or_else(|| link(&check["targetUrl"])),
        });
    }
    Ok(PullRequest {
        number: value["number"]
            .as_u64()
            .ok_or_else(|| GitError("GitHub CLI did not return a pull request number.".into()))?,
        title: required("title")?,
        url: link(&value["url"])
            .ok_or_else(|| GitError("GitHub CLI did not return a web URL.".into()))?,
        state: required("state")?,
        draft: value["isDraft"].as_bool().unwrap_or(false),
        head: required("headRefName")?,
        base: required("baseRefName")?,
        checks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_check_runs_and_legacy_statuses_without_treating_pending_as_passed() {
        let value = serde_json::json!({"number":5,"title":"Example","url":"https://github.com/a/b/pull/5","state":"OPEN","isDraft":true,"headRefName":"feature","baseRefName":"main","statusCheckRollup":[
            {"name":"Build","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://github.com/a/b/actions/runs/1"},
            {"name":"Tests","status":"IN_PROGRESS","conclusion":null},
            {"context":"Legacy","state":"SUCCESS","targetUrl":"https://ci.example.com/1"},
            {"name":"Skip","status":"COMPLETED","conclusion":"SKIPPED"},
            {"name":"Cancel","status":"COMPLETED","conclusion":"CANCELLED","detailsUrl":"javascript:bad"}]});
        let pr = parse(value.to_string().as_bytes()).unwrap();
        assert!(pr.draft);
        assert_eq!(
            pr.checks.iter().map(|c| c.state).collect::<Vec<_>>(),
            [
                State::Failed,
                State::Pending,
                State::Passed,
                State::Skipped,
                State::Cancelled
            ]
        );
        assert!(pr.checks[4].url.is_none());
        assert!(parse(b"{}").is_err());
        assert!(parse(b"not json").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn reads_through_a_process_and_reports_missing_cli_and_auth_errors() {
        use std::os::unix::fs::PermissionsExt;
        let f = crate::git_history::tests::Fixture::new("gh-process");
        let program = f.0.workdir.join("fake-gh");
        let script = "#!/bin/sh\nprintf '%s' '{\"number\":7,\"title\":\"Own CLI fixture\",\"url\":\"https://github.com/a/b/pull/7\",\"state\":\"OPEN\",\"headRefName\":\"feature\",\"baseRefName\":\"main\",\"statusCheckRollup\":[]}'\n";
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(read(&f.0, "feature", &program).unwrap().number, 7);
        std::fs::write(
            &program,
            "#!/bin/sh\necho 'Run gh auth login' >&2\nexit 1\n",
        )
        .unwrap();
        assert!(
            read(&f.0, "feature", &program)
                .unwrap_err()
                .0
                .contains("gh auth login")
        );
        assert!(
            read(&f.0, "feature", &f.0.workdir.join("absent-gh"))
                .unwrap_err()
                .0
                .contains("Install GitHub CLI")
        );
    }
}
