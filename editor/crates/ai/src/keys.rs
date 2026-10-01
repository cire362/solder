//! API keys, kept in the system's secret store: the macOS Keychain through
//! `security`, the Secret Service through `secret-tool` on Linux, and a file
//! readable by its owner only where neither exists. `ANTHROPIC_API_KEY` and
//! `OPENAI_API_KEY` in the environment win over stored keys.
//!
//! Every call blocks on a process or the disk: use a background executor.

use std::{
    collections::BTreeMap,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

const SERVICE: &str = "Solder";

#[derive(Clone, Debug)]
pub struct Keys {
    file: PathBuf,
    system: bool,
}

/// The environment variable a provider's key may come from.
pub fn env_var(account: &str) -> Option<&'static str> {
    match account {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        _ => None,
    }
}

/// Keys go through a command line, so they are limited to what keys use.
fn valid(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 512
        && key
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\' && b != b'\'')
}

impl Keys {
    /// The system store when there is one, else `file`.
    pub fn new(file: PathBuf) -> Self {
        Self { file, system: true }
    }

    /// Only `file`; for tests, which must not touch the real Keychain.
    pub fn file_only(file: PathBuf) -> Self {
        Self {
            file,
            system: false,
        }
    }

    pub fn get(&self, account: &str) -> Option<String> {
        if let Some(var) = env_var(account)
            && let Ok(key) = std::env::var(var)
            && !key.trim().is_empty()
        {
            return Some(key.trim().to_string());
        }
        if self.system
            && let Some(key) = system_get(account)
        {
            return Some(key);
        }
        self.read_file().remove(account)
    }

    /// Where a key comes from, for the UI.
    pub fn source(&self, account: &str) -> Option<&'static str> {
        if let Some(var) = env_var(account)
            && std::env::var(var).is_ok_and(|k| !k.trim().is_empty())
        {
            return Some(var);
        }
        self.get(account).map(|_| "saved")
    }

    pub fn set(&self, account: &str, key: &str) -> Result<(), String> {
        let key = key.trim();
        if !valid(key) {
            return Err("That does not look like an API key".into());
        }
        if self.system && system_set(account, key).is_ok() {
            // A copy left in the file from before would win nowhere, but
            // should not linger either.
            let mut keys = self.read_file();
            if keys.remove(account).is_some() {
                self.write_file(&keys)?;
            }
            return Ok(());
        }
        let mut keys = self.read_file();
        keys.insert(account.to_string(), key.to_string());
        self.write_file(&keys)
    }

    pub fn delete(&self, account: &str) -> Result<(), String> {
        if self.system {
            system_delete(account);
        }
        let mut keys = self.read_file();
        if keys.remove(account).is_some() {
            self.write_file(&keys)?;
        }
        Ok(())
    }

    fn read_file(&self) -> BTreeMap<String, String> {
        std::fs::read(&self.file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write_file(&self, keys: &BTreeMap<String, String>) -> Result<(), String> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let data = serde_json::to_vec_pretty(keys).map_err(|e| e.to_string())?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.file).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            // `mode` only applies when the file is created.
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        file.write_all(&data).map_err(|e| e.to_string())
    }
}

fn output(command: &mut Command) -> Option<String> {
    let out = command.stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn system_get(account: &str) -> Option<String> {
    if cfg!(target_os = "macos") {
        output(Command::new("security").args([
            "find-generic-password",
            "-s",
            SERVICE,
            "-a",
            account,
            "-w",
        ]))
    } else if cfg!(target_os = "linux") {
        output(Command::new("secret-tool").args(["lookup", "service", SERVICE, "account", account]))
    } else {
        None
    }
}

/// Writes the key on standard input, so it never shows in the process list.
fn system_set(account: &str, key: &str) -> Result<(), String> {
    let (mut command, input) = if cfg!(target_os = "macos") {
        let mut c = Command::new("security");
        c.arg("-i");
        (
            c,
            format!("add-generic-password -U -s \"{SERVICE}\" -a \"{account}\" -w \"{key}\"\n"),
        )
    } else if cfg!(target_os = "linux") {
        let mut c = Command::new("secret-tool");
        c.args([
            "store",
            &format!("--label={SERVICE} {account}"),
            "service",
            SERVICE,
            "account",
            account,
        ]);
        (c, key.to_string())
    } else {
        return Err("no system store".into());
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(input.as_bytes())
        .map_err(|e| e.to_string())?;
    let status = child.wait().map_err(|e| e.to_string())?;
    // `security -i` succeeds even when a command fails: read it back.
    if status.success() && system_get(account).as_deref() == Some(key) {
        Ok(())
    } else {
        Err("the system store refused the key".into())
    }
}

fn system_delete(account: &str) {
    let _ = if cfg!(target_os = "macos") {
        Command::new("security")
            .args(["delete-generic-password", "-s", SERVICE, "-a", account])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else if cfg!(target_os = "linux") {
        Command::new("secret-tool")
            .args(["clear", "service", SERVICE, "account", account])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        return;
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_keys_in_a_private_file() {
        let dir = std::env::temp_dir().join(format!("solder-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let keys = Keys::file_only(dir.join("keys.json"));
        assert_eq!(keys.get("acme"), None);
        keys.set("acme", "  sk-acme-123 ").unwrap();
        assert_eq!(keys.get("acme").as_deref(), Some("sk-acme-123"));
        assert_eq!(keys.source("acme"), Some("saved"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("keys.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(keys.set("acme", "has space").is_err());
        assert!(keys.set("acme", "quote\"d").is_err());
        keys.delete("acme").unwrap();
        assert_eq!(keys.get("acme"), None);
        assert_eq!(env_var("anthropic"), Some("ANTHROPIC_API_KEY"));
        assert_eq!(env_var("acme"), None);
    }
}
