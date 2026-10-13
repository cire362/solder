//! A single snapshot from GitHub CLI. Refresh is explicit; no watcher runs
//! in the background and no credentials are copied into the editor.

use crate::git::{GitError, Repo, Result};
use crate::lang::{t, tf};
use std::{path::Path, process::Stdio};

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
            Self::Passed => t("Passed"),
            Self::Failed => t("Failed"),
            Self::Pending => t("Pending"),
            Self::Skipped => t("Skipped"),
            Self::Cancelled => t("Cancelled"),
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

impl PullRequest {
    pub fn state_label(&self) -> &str {
        match self.state.as_str() {
            "OPEN" => t("OPEN"),
            "CLOSED" => t("CLOSED"),
            "MERGED" => t("MERGED"),
            other => other,
        }
    }
}

pub fn read(repo: &Repo, branch: &str, program: &Path) -> Result<PullRequest> {
    let out = read_command(repo, branch, program)
        .and_then(|mut command| {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
        })
        .map_err(|e| {
            GitError(if e.kind() == std::io::ErrorKind::NotFound {
                t("Install GitHub CLI (gh) to read pull requests.").into()
            } else {
                tf("GitHub CLI: {}", &[&e])
            })
        })?;
    if !out.status.success() {
        let message = String::from_utf8_lossy(&out.stderr)
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(t("Could not read the branch's pull request."))
            .to_string();
        return Err(GitError(message));
    }
    parse(&out.stdout)
}

fn read_command(
    repo: &Repo,
    branch: &str,
    program: &Path,
) -> std::io::Result<std::process::Command> {
    let remote = crate::remote::is_remote(&repo.workdir);
    if remote && crate::remote::is_remote(program) {
        let target = crate::git::command_path(&repo.workdir, program)?;
        return read_command_with(repo, branch, Path::new(&target), crate::remote::command);
    }
    read_command_with(repo, branch, program, |cwd, name, args, env| {
        if program.to_str().is_some() {
            crate::remote::command(cwd, name, args, env)
        } else if remote {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                t("Cannot map the remote repository path."),
            ))
        } else {
            // Native executable names may contain non-UTF-8 bytes locally.
            let mut command = std::process::Command::new(program);
            command.current_dir(cwd).args(args);
            for (key, value) in env {
                command.env(key, value);
            }
            Ok(command)
        }
    })
}

fn read_command_with(
    repo: &Repo,
    branch: &str,
    program: &Path,
    launch: impl FnOnce(
        &Path,
        &str,
        &[String],
        &[(String, String)],
    ) -> std::io::Result<std::process::Command>,
) -> std::io::Result<std::process::Command> {
    launch(
        &repo.workdir,
        &program.to_string_lossy(),
        &[
            "pr".into(),
            "view".into(),
            branch.into(),
            "--json".into(),
            "number,title,url,state,isDraft,headRefName,baseRefName,statusCheckRollup".into(),
        ],
        &[
            ("GH_PROMPT_DISABLED".into(), "true".into()),
            ("NO_COLOR".into(), "1".into()),
        ],
    )
}

fn link(value: &serde_json::Value) -> Option<String> {
    let s = value.as_str()?;
    (s.starts_with("https://") || s.starts_with("http://")).then(|| s.into())
}
fn parse(bytes: &[u8]) -> Result<PullRequest> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| GitError(t("GitHub CLI returned unreadable pull request data.").into()))?;
    let required = |field: &str| {
        value[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| GitError(tf("GitHub CLI did not return {}.", &[&field])))
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
                .unwrap_or(t("Check"))
                .into(),
            state,
            url: link(&check["detailsUrl"]).or_else(|| link(&check["targetUrl"])),
        });
    }
    Ok(PullRequest {
        number: value["number"].as_u64().ok_or_else(|| {
            GitError(t("GitHub CLI did not return a pull request number.").into())
        })?,
        title: required("title")?,
        url: link(&value["url"])
            .ok_or_else(|| GitError(t("GitHub CLI did not return a web URL.").into()))?,
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
    fn routing_keeps_the_branch_and_target_environment_raw() {
        let repo = Repo {
            workdir: "/virtual/cache/repo".into(),
        };
        let branch = "feature/virtual-cache-repo";
        let command =
            read_command_with(&repo, branch, Path::new("gh"), |cwd, program, args, env| {
                assert_eq!(cwd, repo.workdir);
                assert_eq!(program, "gh");
                assert_eq!(&args[..4], &["pr", "view", branch, "--json"]);
                assert_eq!(
                    env,
                    &[
                        ("GH_PROMPT_DISABLED".into(), "true".into()),
                        ("NO_COLOR".into(), "1".into())
                    ]
                );
                Ok(std::process::Command::new("fake-ssh"))
            })
            .unwrap();
        assert_eq!(command.get_program(), "fake-ssh");
    }

    #[test]
    fn russian_labels_keep_provider_states_and_names_unchanged() {
        let data = serde_json::json!({
            "number": 5, "title": "Provider title", "url": "https://github.com/a/b/pull/5",
            "state": "OPEN", "headRefName": "feature", "baseRefName": "main",
            "statusCheckRollup": [{"name": "Provider check", "conclusion": "FAILURE"}]
        });
        let mut pr = parse(data.to_string().as_bytes()).unwrap();
        assert_eq!(pr.state_label(), "OPEN");
        assert_eq!(pr.checks[0].state.label(), "Failed");
        crate::lang::set(crate::lang::Language::Russian);
        assert_eq!(pr.state_label(), "Открыт");
        assert_eq!(pr.checks[0].state.label(), "Ошибка");
        assert_eq!(pr.state, "OPEN");
        assert_eq!(pr.title, "Provider title");
        assert_eq!(pr.checks[0].name, "Provider check");
        pr.state = "PROVIDER_STATE".into();
        assert_eq!(pr.state_label(), "PROVIDER_STATE");
        crate::lang::set(crate::lang::Language::English);
        assert_eq!(pr.checks[0].state.label(), "Failed");
    }
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
