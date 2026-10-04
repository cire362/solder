//! Scratch folders for tests in this workspace. They share one folder in the
//! system temp dir, and folders an earlier run left behind are removed once
//! they are an hour old: old enough that no running test still uses them.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime},
};

const STALE: Duration = Duration::from_secs(60 * 60);

/// An empty folder for the test called `name`, unique to this process.
pub fn dir(name: &str) -> PathBuf {
    static SWEPT: OnceLock<()> = OnceLock::new();
    let base = std::env::temp_dir().join("solder-tests");
    SWEPT.get_or_init(|| sweep(&base, SystemTime::now()));
    let dir = base.join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test folder");
    dir
}

fn sweep(base: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|modified| now.duration_since(modified).unwrap_or_default() > STALE);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweeps_only_old_folders() {
        let base = dir("sweep-base");
        std::fs::create_dir_all(base.join("fresh")).unwrap();
        std::fs::create_dir_all(base.join("old")).unwrap();
        // Seen from now neither is old; seen from two hours later both are.
        sweep(&base, SystemTime::now());
        assert!(base.join("fresh").exists() && base.join("old").exists());
        sweep(&base, SystemTime::now() + 2 * STALE);
        assert!(!base.join("fresh").exists() && !base.join("old").exists());
    }
}
