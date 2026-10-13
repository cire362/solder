//! Local saves publish a complete sibling file. The process-wide lock covers
//! revisions and replacement together, including writes through path aliases.

use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::SystemTime,
};

use crate::remote::Stamp;

static WRITES: Mutex<()> = Mutex::new(());
static NEXT: AtomicU64 = AtomicU64::new(0);
const CHANGED: &str = "The file changed; reopen it before saving";

#[derive(Debug)]
enum Refused {
    DanglingLink,
    InvalidPath,
    NotRegular,
    ReadOnly,
    #[cfg(target_os = "macos")]
    SpecialFlags,
    #[cfg(unix)]
    HardLinks,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Attributes,
    #[cfg(unix)]
    Owner,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::DanglingLink => {
                crate::lang::t("The file is a dangling link. Save to another path.")
            }
            Self::InvalidPath => crate::lang::t("Invalid file path"),
            Self::NotRegular => crate::lang::t("The destination is not a regular file"),
            Self::ReadOnly => crate::lang::t("The file is read-only. Save to another path."),
            #[cfg(target_os = "macos")]
            Self::SpecialFlags => crate::lang::t("The file has special flags. Save to a new file."),
            #[cfg(unix)]
            Self::HardLinks => crate::lang::t("The file has hard links. Save to a new file."),
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Attributes => {
                crate::lang::t("Could not preserve file attributes. Save to a new file.")
            }
            #[cfg(unix)]
            Self::Owner => {
                crate::lang::t("Saving would change the file owner. Save to a new file.")
            }
        })
    }
}
impl std::error::Error for Refused {}

fn changed() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, CHANGED)
}

fn stamp(metadata: &Metadata, bytes: &[u8]) -> io::Result<Stamp> {
    revision_stamp(metadata, bytes.len() as u64, ai::install::sha256_hex(bytes))
}

fn revision_stamp(metadata: &Metadata, length: u64, hash: String) -> io::Result<Stamp> {
    Ok(Stamp {
        length,
        mtime: metadata
            .modified()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(u64::MAX as u128) as u64,
        hash: Some(hash),
    })
}

fn agrees(found: &Stamp, expected: &Stamp) -> bool {
    found.length == expected.length
        && found.mtime == expected.mtime
        && expected
            .hash
            .as_ref()
            .is_none_or(|hash| found.hash.as_ref() == Some(hash))
}

fn same_attributes(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.mode() == b.mode()
            && a.uid() == b.uid()
            && a.gid() == b.gid()
            && a.nlink() == b.nlink()
            && a.ctime() == b.ctime()
            && a.ctime_nsec() == b.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        a.permissions().readonly() == b.permissions().readonly()
    }
}

fn identity(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        a.len() == b.len() && a.modified().ok() == b.modified().ok()
    }
}

/// Resolve the parent as well for a missing destination. A dangling symlink
/// is refused rather than replaced with a regular file under its own name.
fn target(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) {
                return Err(io::Error::other(Refused::DanglingLink));
            }
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::other(Refused::InvalidPath))?;
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            Ok(parent.canonicalize()?.join(name))
        }
        Err(error) => Err(error),
    }
}

fn snapshot(path: &Path) -> io::Result<Option<(Stamp, Metadata)>> {
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => return Err(io::Error::other(Refused::NotRegular)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        _ => {}
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(io::Error::other(Refused::NotRegular));
    }
    let (length, hash) = ai::install::sha256_reader(&mut file)?;
    let after = file.metadata()?;
    if before.len() != after.len()
        || after.len() != length
        || before.modified()? != after.modified()?
    {
        return Err(changed());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.ctime() != after.ctime() || before.ctime_nsec() != after.ctime_nsec() {
            return Err(changed());
        }
    }
    let current = fs::metadata(path)?;
    if !identity(&after, &current) {
        return Err(changed());
    }
    Ok(Some((revision_stamp(&after, length, hash)?, after)))
}

pub fn revision(path: &Path) -> io::Result<Option<Stamp>> {
    let _guard = WRITES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    snapshot(&target(path)?).map(|snapshot| snapshot.map(|(stamp, _)| stamp))
}

pub fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    if a == b {
        return Ok(true);
    }
    let (a, b) = match (target(a), target(b)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(error), _) | (_, Err(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(false);
        }
        (Err(error), _) | (_, Err(error)) => return Err(error),
    };
    if a == b {
        return Ok(true);
    }
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => Ok(identity(&a, &b) && cfg!(unix)),
        (Err(e), _) | (_, Err(e)) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(false),
    }
}

struct Staged(PathBuf);
impl Drop for Staged {
    fn drop(&mut self) {
        fs::remove_file(&self.0).ok();
    }
}

fn stage(path: &Path) -> io::Result<(Staged, File)> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other(Refused::InvalidPath))?;
    loop {
        let path = parent.join(format!(
            ".solder-save-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((Staged(path), file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// None means create a new file, not permission to replace an unread one.
pub fn write(path: &Path, bytes: &[u8], expected: Option<&Stamp>) -> io::Result<Stamp> {
    let _guard = WRITES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    write_locked(path, bytes, expected, |_| Ok(()))
}

/// Serialized application state has no editable document revision to protect.
pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<Stamp> {
    let _guard = WRITES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let expected = snapshot(&target(path)?)?.map(|(stamp, _)| stamp);
    write_locked(path, bytes, expected.as_ref(), |_| Ok(()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Created,
    Written,
    Synced,
    Reviewed,
}

fn write_locked(
    path: &Path,
    bytes: &[u8],
    expected: Option<&Stamp>,
    mut check: impl FnMut(Phase) -> io::Result<()>,
) -> io::Result<Stamp> {
    let destination = target(path)?;
    let original = snapshot(&destination)?;
    match (&original, expected) {
        (None, None) => {}
        (Some((found, _)), Some(expected)) if agrees(found, expected) => {}
        _ => return Err(changed()),
    }
    if let Some((_, metadata)) = &original {
        // Rename is controlled by the directory's permissions. An ACL may
        // deny writing the file while still allowing its name to be replaced.
        let writable = OpenOptions::new().write(true).open(&destination)?;
        if !identity(metadata, &writable.metadata()?) {
            return Err(changed());
        }
        if metadata.permissions().readonly() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                Refused::ReadOnly,
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            #[cfg(target_os = "macos")]
            if std::os::darwin::fs::MetadataExt::st_flags(metadata) != 0 {
                return Err(io::Error::other(Refused::SpecialFlags));
            }
            if metadata.nlink() > 1 {
                return Err(io::Error::other(Refused::HardLinks));
            }
        }
    }
    let (staged, mut file) = stage(&destination)?;
    check(Phase::Created)?;
    if original.is_some() {
        // std only carries mode bits. The platform copier also keeps ACLs,
        // extended attributes and, on macOS, resource forks. This blocks only
        // the save worker and runs no shell or extension code.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let mut copy = std::process::Command::new("/bin/cp");
            #[cfg(target_os = "macos")]
            copy.arg("-p");
            #[cfg(target_os = "linux")]
            copy.args([
                "--attributes-only",
                "--preserve=mode,ownership,timestamps,xattr",
            ]);
            let result = copy.arg(&destination).arg(&staged.0).output()?;
            if !result.status.success() {
                return Err(io::Error::other(Refused::Attributes));
            }
            if !identity(&file.metadata()?, &fs::metadata(&staged.0)?) {
                return Err(changed());
            }
            file.set_len(0)?;
        }
    }
    file.write_all(bytes)?;
    check(Phase::Written)?;
    if let Some((_, metadata)) = &original {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let new = file.metadata()?;
            if metadata.uid() != new.uid() || metadata.gid() != new.gid() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    Refused::Owner,
                ));
            }
        }
        file.set_permissions(metadata.permissions())?;
    }
    file.sync_all()?;
    let saved = stamp(&file.metadata()?, bytes)?;
    check(Phase::Synced)?;
    if target(path)? != destination {
        return Err(changed());
    }
    let current = snapshot(&destination)?;
    match (&original, &current) {
        (None, None) => {}
        (Some((old, metadata)), Some((now, current)))
            if agrees(now, old)
                && identity(metadata, current)
                && same_attributes(metadata, current) => {}
        _ => return Err(changed()),
    }
    // Close the staged handle before replacing on Windows. For creation the
    // link operation is exclusive even if another process appears after our
    // check; replacing an existing file still has a final external-writer race.
    let ready = snapshot(&staged.0)?.ok_or_else(changed)?;
    if !identity(&file.metadata()?, &ready.1) || !agrees(&ready.0, &saved) {
        return Err(changed());
    }
    drop(file);
    check(Phase::Reviewed)?;
    if original.is_some() {
        fs::rename(&staged.0, &destination)?;
    } else {
        fs::hard_link(&staged.0, &destination).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                changed()
            } else {
                error
            }
        })?;
    }
    drop(staged);
    #[cfg(unix)]
    {
        if let Some(parent) = destination.parent()
            && let Err(error) = File::open(parent).and_then(|dir| dir.sync_all())
        {
            // Publication already succeeded. Do not report a failed save
            // and leave the document retrying against its old revision.
            crate::journal::error(format_args!(
                "could not sync {} after saving: {error}",
                parent.display()
            ));
        }
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn folder() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "solder-file-save-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn old(path: &Path) -> Stamp {
        fs::write(path, b"old").unwrap();
        revision(path).unwrap().unwrap()
    }
    fn leftovers(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|p| {
                p.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".solder-save-")
            })
            .count()
    }

    #[test]
    fn failures_before_publication_leave_the_original_and_remove_staging() {
        let dir = folder();
        let path = dir.join("file");
        let expected = old(&path);
        for phase in [
            Phase::Created,
            Phase::Written,
            Phase::Synced,
            Phase::Reviewed,
        ] {
            assert!(
                write_locked(&path, b"replacement", Some(&expected), |p| {
                    if p == phase {
                        Err(io::Error::other("injected IO failure"))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), b"old");
            assert_eq!(leftovers(&dir), 0);
        }
    }
    #[test]
    fn a_competing_creator_after_validation_keeps_its_file() {
        let dir = folder();
        let path = dir.join("file");
        assert!(
            write_locked(&path, b"ours", None, |phase| {
                if phase == Phase::Reviewed {
                    fs::write(&path, b"external")?;
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"external");
        assert_eq!(leftovers(&dir), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_acl_denial_is_not_bypassed_by_replacing_the_directory_entry() {
        let dir = folder();
        let path = dir.join("file");
        let expected = old(&path);
        let output = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone deny write"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success());
        let result = write(&path, b"ours", Some(&expected));
        let cleanup = std::process::Command::new("/bin/chmod")
            .arg("-N")
            .arg(&path)
            .status()
            .unwrap();
        assert!(cleanup.success());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(leftovers(&dir), 0);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn extended_attributes_survive_replacement() {
        let dir = folder();
        let path = dir.join("file");
        let expected = old(&path);
        #[cfg(target_os = "linux")]
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg("import os,sys; os.setxattr(sys.argv[1], 'user.solder.fixture', b'kept')")
            .arg(&path)
            .output();
        #[cfg(target_os = "macos")]
        let output = std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "user.solder.fixture", "kept"])
            .arg(&path)
            .output();
        let Ok(output) = output else {
            return;
        };
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let saved = write(&path, b"replacement", Some(&expected)).unwrap();
        assert_eq!(revision(&path).unwrap(), Some(saved));
        #[cfg(target_os = "linux")]
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg("import os,sys; assert os.getxattr(sys.argv[1], 'user.solder.fixture') == b'kept'")
            .arg(&path)
            .output()
            .unwrap();
        #[cfg(target_os = "macos")]
        let output = std::process::Command::new("/usr/bin/xattr")
            .args(["-p", "user.solder.fixture"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        #[cfg(target_os = "macos")]
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "kept");
    }

    #[test]
    fn unknown_sources_and_stale_revisions_never_replace_files() {
        let dir = folder();
        let path = dir.join("file");
        let expected = old(&path);
        assert_eq!(
            write(&path, b"ours", None).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        fs::write(&path, b"new").unwrap();
        assert_eq!(
            write(&path, b"ours", Some(&expected)).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }
    #[test]
    fn a_hash_detects_external_edits_with_identical_length_and_time() {
        let dir = folder();
        let path = dir.join("file");
        let expected = old(&path);
        let time = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"new").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(time))
            .unwrap();
        assert_eq!(
            write(&path, b"ours", Some(&expected)).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }
    #[test]
    fn an_external_write_during_staging_is_not_replaced() {
        let dir = folder();
        let path = dir.join("file");
        let expected = old(&path);
        assert!(
            write_locked(&path, b"ours", Some(&expected), |p| {
                if p == Phase::Synced {
                    fs::write(&path, b"external")?;
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"external");
        assert_eq!(leftovers(&dir), 0);
    }
    #[test]
    fn creating_and_repeatedly_replacing_returns_the_written_revision() {
        let dir = folder();
        let path = dir.join("file");
        let first = write(&path, b"first", None).unwrap();
        assert_eq!(revision(&path).unwrap(), Some(first.clone()));
        let second = write(&path, b"second", Some(&first)).unwrap();
        assert_eq!(revision(&path).unwrap(), Some(second));
        assert_eq!(leftovers(&dir), 0);
        assert!(write(&dir.join("missing/file"), b"bytes", None).is_err());
    }
    #[test]
    fn independent_writers_cannot_both_use_one_revision() {
        let dir = folder();
        let path = dir.join("file");
        let expected = old(&path);
        let threads: Vec<_> = (0..2)
            .map(|i| {
                let path = path.clone();
                let stamp = expected.clone();
                std::thread::spawn(move || {
                    write(&path, format!("writer {i}").as_bytes(), Some(&stamp)).is_ok()
                })
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .filter_map(|t| t.join().ok())
                .filter(|ok| *ok)
                .count(),
            1
        );
    }
    #[cfg(unix)]
    #[test]
    fn saves_follow_links_and_preserve_modes_but_refuse_hardlinks_and_readonly_files() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = folder();
        let path = dir.join("file");
        let expected = old(&path);
        let link = dir.join("link");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&path, &link).unwrap();
        assert!(same_file(&path, &link).unwrap());
        let saved = write(&link, b"ours", Some(&expected)).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::hard_link(&path, dir.join("hard")).unwrap();
        assert!(write(&path, b"bad", Some(&saved)).is_err());
        fs::remove_file(dir.join("hard")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o440)).unwrap();
        assert!(write(&path, b"bad", Some(&saved)).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"ours");
    }
    #[cfg(unix)]
    #[test]
    fn a_retargeted_link_is_not_replaced_or_written_through() {
        use std::os::unix::fs::symlink;
        let dir = folder();
        let path = dir.join("file");
        let expected = old(&path);
        let other = dir.join("other");
        old(&other);
        let link = dir.join("link");
        symlink(&path, &link).unwrap();
        assert!(
            write_locked(&link, b"ours", Some(&expected), |p| {
                if p == Phase::Synced {
                    fs::remove_file(&link)?;
                    symlink(&other, &link)?;
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read(&other).unwrap(), b"old");
        assert_eq!(leftovers(&dir), 0);
    }
}
