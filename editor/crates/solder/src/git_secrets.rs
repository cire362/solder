//! The index is the source of truth. A clean working file must not hide a
//! credential that is still staged, and errors must not let a commit through.

use crate::git::{GitError, Repo, Result};
use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub kind: &'static str,
}

fn rules() -> &'static Vec<(&'static str, Regex)> {
    static RULES: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    RULES.get_or_init(|| [
        ("Private key", r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----"),
        ("AWS access key", r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
        ("GitHub token", r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})\b"),
        ("GitLab token", r"\bglpat-[A-Za-z0-9_-]{20,}\b"),
        ("Slack token", r"\bxox[baprs]-[A-Za-z0-9-]{20,}\b"),
        ("API key", r"\b(?:sk-(?:proj-)?[A-Za-z0-9_-]{24,}|sk_live_[A-Za-z0-9]{16,}|AIza[A-Za-z0-9_-]{35})\b"),
        ("Connection password", r"(?i)\b(?:postgres(?:ql)?|mysql|redis|mongodb(?:\+srv)?)://[^\s/:]+:[^\s/@$]{8,}@"),
    ].into_iter().map(|(name, pattern)| (name, Regex::new(pattern).expect("secret pattern"))).collect())
}

fn credential_assignment(line: &str, path: &str) -> bool {
    static QUOTED: OnceLock<Regex> = OnceLock::new();
    static ENV: OnceLock<Regex> = OnceLock::new();
    let quoted = QUOTED.get_or_init(|| Regex::new(r#"(?i)\b(?:password|passwd|token|api[_-]?key|access[_-]?token|client[_-]?secret|secret[_-]?(?:access[_-]?)?key)\b["']?\s*[:=]\s*["']([^"'\r\n]{16,512})["']"#).expect("credential assignment pattern"));
    let env = ENV.get_or_init(|| Regex::new(r"(?i)^\s*(?:export\s+)?(?:PASSWORD|PASSWD|TOKEN|API_KEY|ACCESS_TOKEN|CLIENT_SECRET|AWS_SECRET_ACCESS_KEY)\s*=\s*([A-Za-z0-9_+/=-]{16,512})\s*$").expect("environment credential pattern"));
    let value = quoted
        .captures(line)
        .or_else(|| {
            std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|name| name.starts_with(".env"))
                .and_then(|_| env.captures(line))
        })
        .and_then(|captures| captures.get(1).map(|m| m.as_str()));
    value.is_some_and(|v| {
        let lower = v.to_ascii_lowercase();
        !v.contains(['$', '<', '{'])
            && !v.contains("...")
            && !["your_", "example", "replace", "placeholder"]
                .iter()
                .any(|p| lower.starts_with(p))
    })
}

pub fn scan(path: &str, bytes: &[u8]) -> Vec<Finding> {
    let text = text::encoding::decode(bytes).text;
    text.lines()
        .enumerate()
        .filter_map(|(ix, line)| {
            rules()
                .iter()
                .find(|(_, rule)| rule.is_match(line))
                .map(|(kind, _)| *kind)
                .or_else(|| credential_assignment(line, path).then_some("Credential value"))
                .map(|kind| Finding {
                    path: path.into(),
                    line: ix + 1,
                    kind,
                })
        })
        .collect()
}

impl Repo {
    pub fn staged_secrets(&self) -> Result<Vec<Finding>> {
        let names = self.run(&[
            "diff",
            "--cached",
            "--name-only",
            "--diff-filter=ACMRT",
            "-z",
            "--",
        ])?;
        let mut found = Vec::new();
        for path in names.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let path = std::str::from_utf8(path).map_err(|_| {
                GitError("Cannot check a staged filename that is not UTF-8.".into())
            })?;
            let spec = format!(":0:{path}");
            let size = self.run(&["cat-file", "-s", &spec])?;
            let size: u64 = String::from_utf8_lossy(&size)
                .trim()
                .parse()
                .map_err(|_| GitError("Cannot read a staged file's size.".into()))?;
            if size > 5 * 1024 * 1024 {
                return Err(GitError(format!(
                    "Cannot check {path}: staged file is over 5 MB. Commit it through Git after checking it."
                )));
            }
            // Gitlinks name commits, not file contents.
            let kind = self.run(&["cat-file", "-t", &spec])?;
            if kind != b"blob\n" {
                continue;
            }
            found.extend(scan(path, &self.run(&["cat-file", "blob", &spec])?));
        }
        Ok(found)
    }

    pub fn check_staged_secrets(&self) -> Result<()> {
        let found = self.staged_secrets()?;
        if found.is_empty() {
            return Ok(());
        }
        // Neither the matching line nor the secret itself reaches the UI.
        let locations: Vec<_> = found
            .iter()
            .take(10)
            .map(|f| format!("{}:{} ({})", f.path, f.line, f.kind))
            .collect();
        Err(GitError(format!(
            "Commit stopped: possible secrets in {}. Remove them from the index and try again.",
            locations.join(", ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::Fixture;
    #[test]
    fn the_index_is_checked_even_when_the_working_file_is_clean() {
        let f = Fixture::new("staged-secret");
        f.write("a.txt", "safe");
        f.0.stage(&["a.txt"]).unwrap();
        f.0.commit("initial", false).unwrap();
        let secret = ["ghp_", &"A".repeat(36)].concat();
        f.write("a.txt", &format!("safe\ntoken={secret}\n"));
        f.0.stage(&["a.txt"]).unwrap();
        f.write("a.txt", "safe");
        let findings = f.0.staged_secrets().unwrap();
        assert_eq!(
            findings,
            [Finding {
                path: "a.txt".into(),
                line: 2,
                kind: "GitHub token"
            }]
        );
        let err = f.0.commit("must stop", false).unwrap_err().0;
        assert!(err.contains("a.txt:2"));
        assert!(!err.contains(&secret));
        assert_eq!(f.git(&["log", "-1", "--format=%s"]).trim(), "initial");
        assert!(f.0.commit("must stop", true).is_err());
        f.0.stage(&["a.txt"]).unwrap();
        f.0.commit("safe amend", true).unwrap();
        assert!(f.0.staged_secrets().unwrap().is_empty());
    }
    #[test]
    fn utf16_and_literal_credentials_are_checked_and_large_blobs_stop() {
        let secret = ["API_KEY=", &"Z".repeat(32)].concat();
        assert_eq!(scan(".env", secret.as_bytes()).len(), 1);
        let quoted = format!("{} = \"{}\"", "password", "randomcredential1234");
        assert_eq!(scan("config.toml", quoted.as_bytes()).len(), 1);
        let bytes = text::encoding::encode(&secret, text::encoding::Encoding::Utf16Le).unwrap();
        assert_eq!(scan(".env", &bytes).len(), 1);
        assert!(
            scan(
                "config.toml",
                b"password = read_configuration\napi_key = \"${LONG_ENVIRONMENT_VARIABLE}\""
            )
            .is_empty()
        );
        let f = Fixture::new("secret-large");
        f.write("large", &"x".repeat(5 * 1024 * 1024 + 1));
        f.0.stage(&["large"]).unwrap();
        assert!(
            f.0.commit("large", false)
                .unwrap_err()
                .0
                .contains("over 5 MB")
        );
    }

    #[test]
    fn detects_private_keys_and_credentials_without_catching_placeholders() {
        let key = ["-----BEGIN ", "RSA PRIVATE KEY-----"].concat();
        let aws = ["AKIA", &"A".repeat(16)].concat();
        let api = ["sk-proj-", &"A".repeat(40)].concat();
        for secret in [
            key,
            aws,
            api,
            ["postgres://user:", "longpassword", "@localhost/db"].concat(),
        ] {
            assert_eq!(scan("config", secret.as_bytes()).len(), 1);
        }
        assert!(
            scan(
                "config",
                b"API_KEY=${API_KEY}\npassword=example\nghp_example\nPUBLIC KEY"
            )
            .is_empty()
        );
    }
}
