//! Downloads an extension and puts it in place.
//!
//! Extensions live under one root: `zed/<id>` and `vscode/<publisher.name>`.
//! A download is unpacked in a staging folder, read, and only then moved to
//! its place, so a broken archive never replaces a working extension.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::{
    Entry, Extension, Origin,
    catalog::{client, describe, runtime, valid_id},
    manifest,
};

/// The largest archive taken. Extensions with a bundled language server
/// reach tens of megabytes; nothing honest is near this.
const DOWNLOAD_LIMIT: u64 = 300 * 1024 * 1024;
/// More files than any extension has; an archive past it is not one.
const FILE_LIMIT: usize = 50_000;

/// Where a download stands, readable at any time.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    cancelled: AtomicBool,
}

impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// 0.0 to 1.0, or `None` while the size is not known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| self.done.load(Ordering::Relaxed) as f32 / total as f32)
    }
}

pub fn dir_for(root: &Path, origin: Origin, id: &str) -> PathBuf {
    root.join(origin.folder()).join(id)
}

/// Every extension under `root`, by name; blocking. A folder that cannot be
/// read is reported and left out.
pub fn installed(root: &Path) -> Vec<Extension> {
    let mut extensions = Vec::new();
    for origin in [Origin::Zed, Origin::VsCode] {
        let Ok(entries) = std::fs::read_dir(root.join(origin.folder())) else {
            continue;
        };
        for dir in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
            match manifest::read(&dir) {
                Ok(extension) => extensions.push(extension),
                Err(e) => eprintln!("extension in {}: {e}", dir.display()),
            }
        }
    }
    extensions.sort_by_key(|e| (e.name.to_lowercase(), e.origin));
    extensions
}

/// Deletes an installed extension; blocking.
pub fn remove(root: &Path, origin: Origin, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err(format!("{id} is not an extension id"));
    }
    match std::fs::remove_dir_all(dir_for(root, origin, id)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

/// Downloads `entry` and installs it under `root`, replacing an older copy.
/// The future can be awaited from any executor.
pub fn install(
    entry: Entry,
    root: PathBuf,
    progress: Arc<Progress>,
) -> impl Future<Output = Result<Extension, String>> + Send + 'static {
    let handle = runtime().spawn(async move {
        if !valid_id(&entry.id) {
            return Err(format!("{} is not an extension id", entry.id));
        }
        let staging = root
            .join(".staging")
            .join(format!("{}-{}", entry.origin.folder(), entry.id));
        let _ = tokio::fs::remove_dir_all(&staging).await;
        let result = stage(&entry, &root, &staging, &progress).await;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        result
    });
    async move { handle.await.map_err(|e| e.to_string())? }
}

async fn stage(
    entry: &Entry,
    root: &Path,
    staging: &Path,
    progress: &Progress,
) -> Result<Extension, String> {
    tokio::fs::create_dir_all(staging)
        .await
        .map_err(|e| e.to_string())?;
    let archive = staging.join("archive");
    let hash = download(&entry.url, &archive, progress).await?;
    if let Some(url) = &entry.sha256_url {
        let published = client()
            .get(url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("Could not check the download: {}", describe(e)))?
            .text()
            .await
            .map_err(describe)?;
        // The file is the hash, or the hash and the archive's name.
        let published = published.split_whitespace().next().unwrap_or_default();
        if !published.eq_ignore_ascii_case(&hash) {
            return Err("The download does not match the SHA-256 its catalog publishes".into());
        }
    }
    let (entry, root, staging) = (entry.clone(), root.to_path_buf(), staging.to_path_buf());
    tokio::task::spawn_blocking(move || place(&entry, &root, &staging, &archive))
        .await
        .map_err(|e| e.to_string())?
}

/// Streams `url` into `dest` and returns its SHA-256 in lowercase hex.
async fn download(url: &str, dest: &Path, progress: &Progress) -> Result<String, String> {
    let mut response = client().get(url).send().await.map_err(describe)?;
    if !response.status().is_success() {
        return Err(format!("The download answered {}", response.status()));
    }
    let total = response.content_length().unwrap_or(0);
    if total > DOWNLOAD_LIMIT {
        return Err("The archive is too large to be an extension".into());
    }
    progress.total.store(total, Ordering::Relaxed);
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    while let Some(chunk) = response.chunk().await.map_err(describe)? {
        if progress.cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        done += chunk.len() as u64;
        if done > DOWNLOAD_LIMIT {
            return Err("The archive is too large to be an extension".into());
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        progress.done.store(done, Ordering::Relaxed);
    }
    file.flush().await.map_err(|e| e.to_string())?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn run(command: &mut Command) -> Result<(), String> {
    let name = command.get_program().to_string_lossy().into_owned();
    let out = command
        .output()
        .map_err(|e| format!("Could not run {name}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Could not unpack: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Unpacks the archive, checks that it is the extension asked for, and moves
/// it to its place; blocking.
fn place(entry: &Entry, root: &Path, staging: &Path, archive: &Path) -> Result<Extension, String> {
    let content = staging.join("content");
    std::fs::create_dir_all(&content).map_err(|e| e.to_string())?;
    let found = match entry.origin {
        // Zed's archives are gzipped tar with the extension at the top.
        Origin::Zed => {
            run(Command::new("tar")
                .arg("-xzf")
                .arg(archive)
                .arg("-C")
                .arg(&content))?;
            content.clone()
        }
        // A .vsix is a zip with the extension in `extension/`.
        Origin::VsCode => {
            let unzip = run(Command::new("unzip")
                .arg("-q")
                .arg("-o")
                .arg(archive)
                .arg("-d")
                .arg(&content));
            if let Err(e) = unzip {
                // Where there is no unzip, the system's tar may read zip.
                run(Command::new("tar")
                    .arg("-xf")
                    .arg(archive)
                    .arg("-C")
                    .arg(&content))
                .map_err(|_| e)?;
            }
            content.join("extension")
        }
    };
    check_tree(&content, &mut 0)?;
    let read = manifest::read(&found)?;
    if read.origin != entry.origin || !read.id.eq_ignore_ascii_case(&entry.id) {
        return Err(format!("The download is {}, not {}", read.id, entry.id));
    }
    let dest = dir_for(root, entry.origin, &entry.id);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    match std::fs::remove_dir_all(&dest) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
        _ => {}
    }
    std::fs::rename(&found, &dest).map_err(|e| e.to_string())?;
    manifest::read(&dest)
}

/// An extension is plain files and folders. A link could point outside its
/// folder, and nothing an extension does needs one.
fn check_tree(dir: &Path, files: &mut usize) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        *files += 1;
        if *files > FILE_LIMIT {
            return Err("The archive has too many files to be an extension".into());
        }
        if kind.is_symlink() {
            return Err("The archive contains a link, which an extension has no use for".into());
        }
        if kind.is_dir() {
            check_tree(&entry.path(), files)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Code;
    use crate::testing::*;

    fn entry(origin: Origin, id: &str, url: String) -> Entry {
        Entry {
            origin,
            id: id.into(),
            name: id.into(),
            version: "1".into(),
            description: String::new(),
            downloads: 0,
            provides: Vec::new(),
            url,
            sha256_url: None,
        }
    }

    #[test]
    fn installs_a_zed_extension_and_replaces_the_older_copy() {
        let dir = scratch("zed");
        zed_extension(&dir.join("source"));
        let (base, _) = serve(vec![
            (
                "/extensions/demo/download",
                Served::redirect("/files/demo.tgz"),
            ),
            ("/files/demo.tgz", Served::ok(tar(&dir.join("source")))),
        ]);
        let root = dir.join("root");
        // Something left by an older version must not survive.
        write(&root.join("zed/demo/stale.txt"), "old");
        let progress = Progress::new();
        let installed = block(install(
            entry(
                Origin::Zed,
                "demo",
                format!("{base}/extensions/demo/download"),
            ),
            root.clone(),
            progress.clone(),
        ))
        .unwrap();
        assert_eq!(progress.fraction(), Some(1.0));
        let home = root.join("zed/demo");
        assert_eq!(installed.dir, home);
        assert!(!home.join("stale.txt").exists());
        assert!(!root.join(".staging/zed-demo").exists());

        assert_eq!(
            (installed.name.as_str(), installed.version.as_str()),
            ("Demo", "1.2.0")
        );
        assert_eq!(
            installed.code,
            Code::Zed {
                api: "0.7.0".into()
            }
        );
        let names: Vec<&str> = installed.themes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Demo Dark", "Demo Light"]);
        assert_eq!(installed.themes[0].syntax["keyword"], "#ff8800");

        // One language has its grammar, the other names one that is absent.
        assert_eq!(installed.languages.len(), 2);
        let demo = &installed.languages[0];
        assert_eq!(demo.name, "Demo");
        assert_eq!(demo.suffixes, ["demo", "Demofile"]);
        assert_eq!(demo.aliases, ["dm"]);
        assert_eq!(demo.line_comment.as_deref(), Some("//"));
        let grammar = demo.grammar.as_ref().unwrap();
        assert_eq!(grammar.symbol, "demo");
        assert_eq!(grammar.module, home.join("grammars/demo.wasm"));
        assert_eq!(
            grammar.highlights,
            Some(home.join("languages/demo/highlights.scm"))
        );
        assert!(installed.languages[1].grammar.is_none());

        assert_eq!(installed.servers.len(), 1);
        assert_eq!(installed.servers[0].name, "Demo LS");
        assert_eq!(installed.servers[0].languages, ["Demo", "Demo Template"]);
        assert_eq!(
            installed.servers[0].language_ids,
            [("Demo Template".to_string(), "demo-template".to_string())]
        );

        let snippets: Vec<_> = installed
            .snippets
            .iter()
            .map(|s| {
                (
                    s.path.strip_prefix(&home).unwrap().to_str().unwrap(),
                    s.languages.clone(),
                )
            })
            .collect();
        assert_eq!(
            snippets,
            [
                ("snippets/javascript.json", vec!["javascript".to_string()]),
                ("extra/all.json", vec!["all".to_string()]),
            ]
        );
        assert_eq!(installed.missing, ["Icon themes"]);
        assert_eq!(
            installed.commands,
            [("demo-ls".to_string(), vec!["--version".to_string()])]
        );
        assert_eq!(
            installed.provides(),
            "1 language, 2 themes, 2 snippet files"
        );

        assert_eq!(super::installed(&root), vec![installed]);
        remove(&root, Origin::Zed, "demo").unwrap();
        assert!(super::installed(&root).is_empty());
        remove(&root, Origin::Zed, "demo").unwrap();
        assert!(remove(&root, Origin::Zed, "../x").is_err());
    }

    #[test]
    fn installs_a_vscode_extension_and_checks_its_hash() {
        let dir = scratch("vscode");
        vscode_extension(&dir.join("source/extension"));
        write(&dir.join("source/outside.json"), "{}");
        let Some(vsix) = zip(&dir.join("source"), "extension") else {
            eprintln!("skipped: no python3 to build a .vsix");
            return;
        };
        let hash: String = Sha256::digest(&vsix)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let (base, _) = serve(vec![
            ("/demo.vsix", Served::ok(vsix)),
            (
                "/demo.sha256",
                Served::ok(format!("{}  demo.vsix\n", hash.to_uppercase()).into_bytes()),
            ),
            ("/wrong.sha256", Served::ok(b"00ff".to_vec())),
        ]);
        let root = dir.join("root");
        let mut wanted = entry(Origin::VsCode, "acme.demo", format!("{base}/demo.vsix"));

        wanted.sha256_url = Some(format!("{base}/wrong.sha256"));
        let error = block(install(wanted.clone(), root.clone(), Progress::new())).unwrap_err();
        assert_eq!(
            error,
            "The download does not match the SHA-256 its catalog publishes"
        );
        assert!(!root.join("vscode").exists());

        wanted.sha256_url = Some(format!("{base}/demo.sha256"));
        let installed = block(install(wanted, root.clone(), Progress::new())).unwrap();
        // The folder is named as the catalog names it; the id is the manifest's.
        assert_eq!(installed.dir, root.join("vscode/acme.demo"));
        assert_eq!(installed.id, "Acme.demo");
        assert_eq!(installed.name, "Acme Demo");
        assert_eq!(installed.code, Code::Node);
        assert_eq!(installed.themes.len(), 1);
        assert_eq!(installed.themes[0].name, "Acme Dark");
        // One file for two languages; the one outside the folder is ignored.
        assert_eq!(installed.snippets.len(), 1);
        assert_eq!(
            installed.snippets[0].languages,
            ["javascript", "typescriptreact"]
        );
        let language = &installed.languages[0];
        assert_eq!(language.name, "Demo Lang");
        assert_eq!(language.suffixes, ["dm", "Demofile"]);
        assert_eq!(language.aliases, ["demo", "Demo Lang"]);
        assert_eq!(language.line_comment.as_deref(), Some("#"));
        assert!(language.grammar.is_none());
        assert_eq!(
            installed.missing,
            [
                "1 of its themes (not in the JSON format)",
                "Highlighting (a TextMate grammar)",
                "Its code, which needs VS Code",
                "Key bindings for its commands",
            ]
        );
        assert_eq!(installed.provides(), "1 theme, 1 snippet file");
    }

    #[test]
    fn refuses_what_is_not_the_extension_asked_for() {
        let dir = scratch("refuse");
        zed_extension(&dir.join("source"));
        let good = tar(&dir.join("source"));
        std::os::unix::fs::symlink("/etc/hosts", dir.join("source/themes/hosts.json")).unwrap();
        let linked = tar(&dir.join("source"));
        let (base, _) = serve(vec![
            ("/good.tgz", Served::ok(good)),
            ("/linked.tgz", Served::ok(linked)),
            ("/text", Served::ok(b"not an archive".to_vec())),
        ]);
        let root = dir.join("root");
        let try_install = |id: &str, path: &str| {
            block(install(
                entry(Origin::Zed, id, format!("{base}{path}")),
                root.clone(),
                Progress::new(),
            ))
            .unwrap_err()
        };
        assert_eq!(
            try_install("other", "/good.tgz"),
            "The download is demo, not other"
        );
        assert_eq!(
            try_install("demo", "/linked.tgz"),
            "The archive contains a link, which an extension has no use for"
        );
        assert!(try_install("demo", "/text").starts_with("Could not unpack"));
        assert_eq!(
            try_install("demo", "/nowhere"),
            "The download answered 404 Not Found"
        );
        assert_eq!(
            try_install("../demo", "/good.tgz"),
            "../demo is not an extension id"
        );
        assert!(!root.join("zed").exists());
        assert!(
            std::fs::read_dir(root.join(".staging"))
                .unwrap()
                .next()
                .is_none()
        );

        let cancelled = Progress::new();
        cancelled.cancel();
        let error = block(install(
            entry(Origin::Zed, "demo", format!("{base}/good.tgz")),
            root.clone(),
            cancelled,
        ))
        .unwrap_err();
        assert_eq!(error, "Cancelled");
    }
}
