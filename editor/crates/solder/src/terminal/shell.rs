//! Reports completed interactive commands through a bounded, per-launch OSC title.
//!
//! `prepare` does filesystem IO and belongs on the background executor. Keep its
//! result alive for the PTY's lifetime; `report` only parses memory and is UI-safe.
//! Bash uses history and skips stale/disabled/ignored entries. Commands which
//! rewrite history, or shell hooks installed after ours, can defeat attribution.
//! Login Bash temporarily changes HOME for global /etc/profile, restoring it
//! before user profiles; login_shell and normal logout behavior remain intact.
//! Zsh restores user ZDOTDIR for each user file and before the first prompt. Global
//! startup files that change HOME/ZDOTDIR or disable RCS can bypass the wrappers.
//! On macOS an implicit shell is executed directly with -l; /usr/bin/login's
//! session accounting and messages are not reproduced. Missing SHELL opts out.
//! The token distinguishes sessions, not hostile programs inside the same PTY.

use std::{io, path::PathBuf};

use serde::{Deserialize, Serialize};

const PREFIX: &str = "solder-command;";
const MAX_JSON_BYTES: usize = 16 * 1024;
const MAX_COMMAND_BYTES: usize = 8 * 1024;
const MAX_CWD_BYTES: usize = 4096;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub command: String,
    pub cwd: PathBuf,
    pub status: i32,
}

pub struct Integration {
    folder: PathBuf,
    token: String,
}

impl Integration {
    pub fn report(&self, title: &str) -> Option<Report> {
        if title.len() > MAX_JSON_BYTES + PREFIX.len() + self.token.len() + 1 {
            return None;
        }
        let json = title
            .strip_prefix(PREFIX)?
            .strip_prefix(&self.token)?
            .strip_prefix(';')?;
        if json.len() > MAX_JSON_BYTES {
            return None;
        }
        let report: Report = serde_json::from_str(json).ok()?;
        let cwd = report.cwd.to_str()?;
        (report.status >= 0
            && report.status <= 255
            && !report.command.trim().is_empty()
            && report.command.len() <= MAX_COMMAND_BYTES
            && !report.command.contains('\0')
            && report.cwd.is_absolute()
            && cwd.len() <= MAX_CWD_BYTES
            && !cwd.contains('\0'))
        .then_some(report)
    }
}

impl Drop for Integration {
    fn drop(&mut self) {
        let folder = self.folder.clone();
        // Dropping a terminal runs on the UI thread, unlike filesystem cleanup.
        let _ = std::thread::Builder::new()
            .name("shell-cleanup".into())
            .spawn(move || {
                let _ = std::fs::remove_dir_all(folder);
            });
    }
}

#[cfg(not(unix))]
pub fn prepare(_command: &mut super::TerminalCommand) -> io::Result<Option<Integration>> {
    Ok(None)
}

#[cfg(unix)]
pub fn prepare(command: &mut super::TerminalCommand) -> io::Result<Option<Integration>> {
    use std::{
        env, fs,
        os::unix::fs::DirBuilderExt,
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let implicit = command.program.is_none();
    let Some(program) = command.program.clone().or_else(|| {
        command
            .env
            .get("SHELL")
            .cloned()
            .or_else(|| env::var("SHELL").ok())
    }) else {
        return Ok(None);
    };
    let Some(shell @ ("bash" | "zsh")) = Path::new(&program).file_name().and_then(|s| s.to_str())
    else {
        return Ok(None);
    };
    let Some(mut login) = login_flags(&command.args) else {
        return Ok(None);
    };
    // Alacritty's implicit shell is a login session on macOS, but not on Linux.
    login |= implicit && cfg!(target_os = "macos");
    let home = command
        .env
        .get("HOME")
        .cloned()
        .or_else(|| env::var("HOME").ok());
    if shell == "bash" && login && home.as_deref().is_none_or(str::is_empty) {
        return Ok(None);
    }

    let token = format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let folder = env::temp_dir().join(format!("solder-shell-{token}"));
    fs::DirBuilder::new().mode(0o700).create(&folder)?;
    let integration = Integration { folder, token };
    let folder = integration
        .folder
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Shell hook path is not UTF-8"))?
        .to_owned();
    fs::write(
        integration.folder.join("json.sh"),
        include_str!("shell/json.sh"),
    )?;

    let mut args = command.args.clone();
    let mut env = command.env.clone();
    env.insert("SOLDER_SHELL_TOKEN".into(), integration.token.clone());
    env.insert("SOLDER_SHELL_DIR".into(), folder.clone());
    if login && !login_flags(&args).unwrap_or(false) {
        args.push("-l".into());
    }
    args.push("-i".into());
    if shell == "bash" {
        fs::write(
            integration.folder.join("bash.sh"),
            include_str!("shell/bash.sh"),
        )?;
        fs::write(
            integration.folder.join("bashrc"),
            include_str!("shell/bashrc"),
        )?;
        if login {
            fs::write(
                integration.folder.join(".bash_profile"),
                include_str!("shell/bashrc"),
            )?;
            env.insert("SOLDER_SHELL_HOME".into(), home.unwrap());
            env.insert("HOME".into(), folder);
        } else {
            args.splice(0..0, ["--rcfile".into(), format!("{folder}/bashrc")]);
        }
    } else {
        for (name, source) in [
            (".zshenv", include_str!("shell/zshenv")),
            (".zprofile", include_str!("shell/zprofile")),
            (".zshrc", include_str!("shell/zshrc")),
            (".zlogin", include_str!("shell/zlogin")),
            ("zsh.sh", include_str!("shell/zsh.sh")),
        ] {
            fs::write(integration.folder.join(name), source)?;
        }
        let zdotdir = command
            .env
            .get("ZDOTDIR")
            .cloned()
            .or_else(|| env::var("ZDOTDIR").ok());
        env.insert(
            "SOLDER_SHELL_ZDOTDIR_SET".into(),
            if zdotdir.is_some() { "1" } else { "0" }.into(),
        );
        env.insert("SOLDER_SHELL_ZDOTDIR".into(), zdotdir.unwrap_or_default());
        env.insert("ZDOTDIR".into(), folder);
    }
    // Failure above leaves the caller's command unchanged and drops the folder.
    command.program = Some(program);
    command.args = args;
    command.env = env;
    Ok(Some(integration))
}

#[cfg(unix)]
fn login_flags(args: &[String]) -> Option<bool> {
    let mut login = false;
    for arg in args {
        if arg == "--login" {
            login = true;
        } else {
            let flags = arg.strip_prefix('-')?;
            if flags.is_empty() || !flags.chars().all(|c| c == 'i' || c == 'l') {
                return None;
            }
            login |= flags.contains('l');
        }
    }
    Some(login)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reports_require_exact_token_and_valid_fields() {
        let integration = Integration {
            folder: PathBuf::new(),
            token: "test-token".into(),
        };
        let title = |json: &str| format!("{PREFIX}test-token;{json}");
        let cwd = if cfg!(windows) {
            r"C:\tmp\a b"
        } else {
            "/tmp/a b"
        };
        let json = serde_json::to_string(&Report {
            command: "printf 'a\\b\"c\n'; false".into(),
            cwd: cwd.into(),
            status: 1,
        })
        .unwrap();
        let report = integration.report(&title(&json)).unwrap();
        assert_eq!(report.status, 1);
        assert_eq!(report.cwd, PathBuf::from(cwd));
        assert!(
            integration
                .report(&title(&json).replace("test-token;", "test-token-extra;"))
                .is_none()
        );
        assert!(integration.report(&format!("normal title{json}")).is_none());
        for invalid in [
            r#"{"command":"false","cwd":"/tmp","status":-1}"#,
            r#"{"command":"false","cwd":"/tmp","status":256}"#,
            r#"{"command":"false","cwd":"/tmp","status":1.0}"#,
            r#"{"command":" ","cwd":"/tmp","status":1}"#,
            r#"{"command":"false","cwd":"relative","status":1}"#,
            r#"{"command":"false","cwd":"/tmp","status":1,"token":"x"}"#,
            r#"{"command":"false","command":"true","cwd":"/tmp","status":1}"#,
            r#"{"command":"false\u0000","cwd":"/tmp","status":1}"#,
            r#"{"command":"false","cwd":"/tmp","status":1} trailing"#,
        ] {
            assert!(integration.report(&title(invalid)).is_none(), "{invalid}");
        }
        assert!(
            integration
                .report(&title(&"x".repeat(MAX_JSON_BYTES + 1)))
                .is_none()
        );
        let large = serde_json::to_string(&Report {
            command: "x".repeat(MAX_COMMAND_BYTES + 1),
            cwd: "/tmp".into(),
            status: 1,
        })
        .unwrap();
        assert!(integration.report(&title(&large)).is_none());
        // A parser-only fixture owns no directory to clean up.
        std::mem::forget(integration);
    }

    #[cfg(unix)]
    #[test]
    fn only_plain_interactive_shells_are_prepared() {
        for args in [
            vec!["-c", "false"],
            vec!["-lic", "false"],
            vec!["script.sh"],
            vec!["--norc"],
            vec!["-s"],
            vec!["--"],
        ] {
            let mut command = super::super::TerminalCommand {
                program: Some("/bin/bash".into()),
                args: args.iter().map(|s| (*s).into()).collect(),
                ..Default::default()
            };
            let before = command.args.clone();
            assert!(prepare(&mut command).unwrap().is_none());
            assert_eq!(command.args, before);
            assert!(command.env.is_empty());
        }
        assert_eq!(login_flags(&["-il".into()]), Some(true));
        assert_eq!(login_flags(&["-i".into()]), Some(false));
        let mut command = super::super::TerminalCommand {
            program: Some("/bin/sh".into()),
            ..Default::default()
        };
        assert!(prepare(&mut command).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn interactive_commands_are_reported_from_real_ptys() {
        // Python supplies a real controlling PTY without depending on the parent
        // wiring prepare into Terminal::start (which would prepare twice here).
        let output = match std::process::Command::new("python3")
            .arg("-c")
            .arg(include_str!("shell/pty_test.py"))
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/terminal/shell"))
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(error) => panic!("Could not run shell PTY tests: {error}"),
        };
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
