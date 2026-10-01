//! Downloads llama.cpp's server and model files.
//!
//! The server build is pinned with the SHA-256 GitHub publishes for it. A
//! model's size and SHA-256 come from Hugging Face (the `X-Linked-*` headers
//! of the file's URL). Both are checked while the file streams in, so a
//! truncated or substituted file never reaches its final name. An
//! interrupted download resumes from its `.part` file.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, atomic::Ordering},
};

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::{
    Dirs, Model, Progress,
    catalog::Source,
    client,
    hardware::{Backend, Hardware},
};

/// The llama.cpp build Solder runs. Raising it means updating every hash.
pub const LLAMA_BUILD: &str = "b11312";
pub const HUB: &str = "https://huggingface.co";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// The server build for this machine: Metal on Apple Silicon, Vulkan where
/// there is a GPU, the CPU build otherwise.
pub fn runtime_asset(hw: &Hardware) -> Option<Asset> {
    let (name, size, sha256) = match (hw.os, hw.arch, hw.backend) {
        ("macos", "aarch64", _) => (
            "macos-arm64.tar.gz",
            11_820_971,
            "3ceea603e48367ab694822f7304e6880817337f942f9672321411932c76c46f2",
        ),
        ("macos", "x86_64", _) => (
            "macos-x64.tar.gz",
            11_375_343,
            "c78ebb522375db1675d5df252b0a36dbec8822425852673a5f22c8c9eaa23b86",
        ),
        ("linux", "x86_64", Backend::Vulkan) => (
            "ubuntu-vulkan-x64.tar.gz",
            31_480_843,
            "81b207361c95483e861763944cd371f916964a9d5626d649919690e6860e744a",
        ),
        ("linux", "x86_64", _) => (
            "ubuntu-x64.tar.gz",
            17_537_177,
            "c250f4a85fb736b92ab369e531c2c769af4ad6180718c72ac0350e26678008de",
        ),
        ("linux", "aarch64", Backend::Vulkan) => (
            "ubuntu-vulkan-arm64.tar.gz",
            24_740_886,
            "993e4ac8ab646351d52e3da7e1e987f78015cb006e0ce7f07ef5c925b8b7ad74",
        ),
        ("linux", "aarch64", _) => (
            "ubuntu-arm64.tar.gz",
            13_581_259,
            "fb665e51a07d65156d0a4a327b318fe068184557c499772788049a6660106262",
        ),
        ("windows", "x86_64", Backend::Vulkan) => (
            "win-vulkan-x64.zip",
            33_196_055,
            "ad3e1e83bdcbc56c00fb2d740e8799e3012d63a12d0a64509407e625a77378a0",
        ),
        ("windows", "x86_64", _) => (
            "win-cpu-x64.zip",
            19_265_352,
            "d2d966c23e4d097d70633f9d92e4931245b575be7a5409201d8f352493f0e702",
        ),
        ("windows", "aarch64", _) => (
            "win-cpu-arm64.zip",
            12_108_942,
            "1822a499587503423f65cc9348253d6f883c799116becdb36baa3f9e14390c2d",
        ),
        _ => return None,
    };
    Some(Asset {
        url: format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/{LLAMA_BUILD}/llama-{LLAMA_BUILD}-bin-{name}"
        ),
        size,
        sha256: sha256.into(),
    })
}

fn server_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

/// The installed server, if the pinned build is there.
pub fn installed_runtime(dirs: &Dirs) -> Option<PathBuf> {
    find_file(&dirs.runtime().join(LLAMA_BUILD), server_name())
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if entry.file_name() == name {
            return Some(path);
        }
    }
    None
}

/// Downloads and unpacks the server; returns its path.
pub async fn install_runtime(
    dirs: Dirs,
    asset: Asset,
    progress: Arc<Progress>,
) -> Result<PathBuf, String> {
    let root = dirs.runtime();
    let archive_name = asset
        .url
        .rsplit('/')
        .next()
        .unwrap_or("llama.tar.gz")
        .to_string();
    let archive = root.join(&archive_name);
    download(&asset, &archive, &progress).await?;
    let target = root.join(LLAMA_BUILD);
    let unpack = {
        let (archive, target) = (archive.clone(), target.clone());
        tokio::task::spawn_blocking(move || unpack(&archive, &target))
    };
    let result = unpack.await.map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&archive);
    result?;
    // Older builds are not used again.
    for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        if entry.path() != target && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    installed_runtime(&dirs).ok_or_else(|| "The download has no llama-server".to_string())
}

/// `tar` reads both the `.tar.gz` builds and, on Windows, the `.zip` ones.
pub fn unpack(archive: &Path, target: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(target);
    std::fs::create_dir_all(target).map_err(|e| e.to_string())?;
    let out = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(target)
        .output()
        .map_err(|e| format!("Could not run tar: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(target);
        return Err(format!(
            "Could not unpack: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// A file's URL on Hugging Face.
pub fn hub_url(hub: &str, repo: &str, file: &str) -> String {
    format!("{hub}/{repo}/resolve/main/{file}")
}

/// A model file's URL, size and SHA-256 on Hugging Face.
pub async fn model_asset(hub: &str, repo: &str, file: &str) -> Result<Asset, String> {
    let url = hub_url(hub, repo, file);
    // The size and hash come with the redirect to the file's storage.
    let head = no_redirects()
        .head(&url)
        .send()
        .await
        .map_err(|e| format!("Could not reach Hugging Face: {e}"))?;
    let header = |name: &str| {
        head.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim_matches('"').to_string())
    };
    let (Some(size), Some(sha256)) = (header("x-linked-size"), header("x-linked-etag")) else {
        return Err(format!(
            "Hugging Face has no {file} in {repo} (HTTP {})",
            head.status().as_u16()
        ));
    };
    let size = size
        .parse()
        .map_err(|_| "Bad size from Hugging Face".to_string())?;
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Hugging Face gave no SHA-256 for the file".into());
    }
    Ok(Asset { url, size, sha256 })
}

fn no_redirects() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect_policy(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("Solder/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client")
    })
}

/// Downloads a model into the models directory; returns its path. A model
/// already on disk is used where it is.
pub async fn install_model(
    dirs: Dirs,
    hub: String,
    model: Model,
    progress: Arc<Progress>,
) -> Result<PathBuf, String> {
    let path = dirs.model(&model);
    let Source::Hub { repo, file } = &model.source else {
        return Ok(path);
    };
    let asset = model_asset(&hub, repo, file).await?;
    download(&asset, &path, &progress).await?;
    Ok(path)
}

/// The ids of `models` whose files are on disk.
pub fn installed_models(dirs: &Dirs, models: &[Model]) -> Vec<String> {
    models
        .iter()
        .filter(|m| dirs.model(m).is_file())
        .map(|m| m.id.clone())
        .collect()
}

/// Deletes a downloaded model. Files Solder did not download stay.
pub fn remove_model(dirs: &Dirs, model: &Model) -> std::io::Result<()> {
    if !matches!(model.source, Source::Hub { .. }) {
        return Ok(());
    }
    let path = dirs.model(model);
    let _ = std::fs::remove_file(part_path(&path));
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// Streams `asset` to `dest`, resuming a `.part` file left by an earlier
/// attempt, and checks its size and SHA-256 before giving it its name.
pub async fn download(asset: &Asset, dest: &Path, progress: &Progress) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(free) = crate::free_space(dest)
        && free < asset.size
    {
        return Err(format!(
            "Not enough disk space: {} needed, {} free",
            crate::format_size(asset.size),
            crate::format_size(free)
        ));
    }
    let part = part_path(dest);
    progress.total.store(asset.size, Ordering::Relaxed);

    // Hash what an earlier attempt already wrote.
    let mut hasher = Sha256::new();
    let mut have = 0u64;
    if let Ok(existing) = std::fs::metadata(&part)
        && existing.len() <= asset.size
    {
        let path = part.clone();
        let (h, n) = tokio::task::spawn_blocking(move || -> std::io::Result<(Sha256, u64)> {
            let mut hasher = Sha256::new();
            let mut file = std::fs::File::open(path)?;
            let mut buf = vec![0; 1 << 20];
            let mut n = 0;
            loop {
                let read = file.read(&mut buf)?;
                if read == 0 {
                    return Ok((hasher, n));
                }
                hasher.update(&buf[..read]);
                n += read as u64;
            }
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        hasher = h;
        have = n;
    }

    let mut file;
    if have < asset.size {
        let mut request = client().get(&asset.url);
        if have > 0 {
            request = request.header("Range", format!("bytes={have}-"));
        }
        let mut response = request.send().await.map_err(describe)?;
        let status = response.status();
        if have > 0 && status == reqwest::StatusCode::OK {
            // The server ignored the range: start over.
            hasher = Sha256::new();
            have = 0;
        } else if !status.is_success() {
            return Err(format!("Download failed: HTTP {}", status.as_u16()));
        }
        file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(have > 0)
            .truncate(have == 0)
            .open(&part)
            .await
            .map_err(|e| e.to_string())?;
        progress.done.store(have, Ordering::Relaxed);
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(e) => {
                    // Keep what arrived, so the next attempt resumes there.
                    file.flush().await.ok();
                    return Err(describe(e));
                }
            };
            if progress.is_cancelled() {
                // The part stays, so the next attempt resumes.
                file.flush().await.ok();
                return Err("Cancelled".into());
            }
            have += chunk.len() as u64;
            if have > asset.size {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err("The download is larger than expected".into());
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            progress.done.store(have, Ordering::Relaxed);
        }
        file.flush().await.map_err(|e| e.to_string())?;
        file.sync_all().await.map_err(|e| e.to_string())?;
    }
    if have != asset.size {
        return Err(format!(
            "The download stopped at {} of {}; try again to resume",
            crate::format_size(have),
            crate::format_size(asset.size)
        ));
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !digest.eq_ignore_ascii_case(&asset.sha256) {
        let _ = std::fs::remove_file(&part);
        return Err("The download does not match its SHA-256 and was deleted".into());
    }
    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| e.to_string())
}

fn describe(e: reqwest::Error) -> String {
    if e.is_connect() {
        "Could not connect; check the network".into()
    } else if e.is_timeout() {
        "The download stalled; try again to resume".into()
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::atomic::AtomicUsize,
    };

    fn sha(data: &[u8]) -> String {
        Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Serves `body` with Range support, `/meta` as Hugging Face's redirect
    /// with size and hash, and cuts the first full response after `cut`
    /// bytes. Returns the base URL and the number of requests seen.
    fn server(body: Vec<u8>, cut: Option<usize>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(AtomicUsize::new(0));
        let count = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let n = count.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut range = None;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                if first.contains("/meta") {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 302 Found\r\nLocation: /file\r\nX-Linked-Size: {}\r\nX-Linked-Etag: \"{}\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        body.len(),
                        sha(&body)
                    );
                    continue;
                }
                let start = range.unwrap_or(0);
                let rest = &body[start..];
                let status = if range.is_some() {
                    "206 Partial Content"
                } else {
                    "200 OK"
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    rest.len()
                );
                let send = match cut {
                    Some(c) if n == 0 => &rest[..c],
                    _ => rest,
                };
                let _ = stream.write_all(send);
            }
        });
        (base, seen)
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("solder-ai-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn block<T>(
        f: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
    ) -> Result<T, String>
    where
        T: Send + 'static,
    {
        crate::runtime().block_on(f)
    }

    #[test]
    fn downloads_resumes_and_checks_the_hash() {
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (base, seen) = server(body.clone(), Some(100_000));
        let asset = Asset {
            url: format!("{base}/file"),
            size: body.len() as u64,
            sha256: sha(&body),
        };
        let dest = dir("resume").join("model.gguf");
        let progress = Progress::new();
        // The first response is cut short; the part file stays.
        let (a, d, p) = (asset.clone(), dest.clone(), progress.clone());
        let first = block(async move { download(&a, &d, &p).await });
        assert!(first.is_err(), "{first:?}");
        assert!(!dest.exists());
        assert_eq!(std::fs::metadata(part_path(&dest)).unwrap().len(), 100_000);
        // The second resumes from there.
        let (a, d, p) = (asset.clone(), dest.clone(), progress.clone());
        block(async move { download(&a, &d, &p).await }).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(!part_path(&dest).exists());
        assert_eq!(progress.fraction(), Some(1.0));
        assert_eq!(seen.load(Ordering::SeqCst), 2);

        // A file that does not match its hash is deleted.
        let bad = Asset {
            sha256: "0".repeat(64),
            ..asset
        };
        let dest = dest.with_file_name("bad.gguf");
        let (d, p) = (dest.clone(), Progress::new());
        let err = block(async move { download(&bad, &d, &p).await }).unwrap_err();
        assert!(err.contains("SHA-256"), "{err}");
        assert!(!dest.exists() && !part_path(&dest).exists());
    }

    #[test]
    fn cancelling_keeps_the_part() {
        let body = vec![7u8; 4_000_000];
        let (base, _) = server(body.clone(), None);
        let asset = Asset {
            url: format!("{base}/file"),
            size: body.len() as u64,
            sha256: sha(&body),
        };
        let dest = dir("cancel").join("m.gguf");
        let progress = Progress::new();
        progress.cancel();
        let (d, p) = (dest.clone(), progress.clone());
        let err = block(async move { download(&asset, &d, &p).await }).unwrap_err();
        assert_eq!(err, "Cancelled");
        assert!(!dest.exists());
    }

    #[test]
    fn reads_model_size_and_hash_from_the_hub() {
        let body = b"GGUF fake model".to_vec();
        let (base, _) = server(body.clone(), None);
        let hub = base.clone();
        let asset = block(async move { model_asset(&hub, "org/repo", "meta").await }).unwrap();
        assert_eq!(asset.size, body.len() as u64);
        assert_eq!(asset.sha256, sha(&body));
        assert_eq!(asset.url, format!("{base}/org/repo/resolve/main/meta"));
    }

    #[test]
    fn unpacks_a_build_and_finds_the_server() {
        let root = dir("unpack");
        let src = root.join("src/llama-b1");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join(server_name()), "#!/bin/sh\n").unwrap();
        let archive = root.join("build.tar.gz");
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(root.join("src"))
            .arg("llama-b1")
            .status()
            .unwrap();
        assert!(ok.success());
        let dirs = Dirs::new(root.join("data"));
        unpack(&archive, &dirs.runtime().join(LLAMA_BUILD)).unwrap();
        let found = installed_runtime(&dirs).unwrap();
        assert!(found.ends_with(Path::new("llama-b1").join(server_name())));
        assert!(unpack(&root.join("missing.tar.gz"), &root.join("x")).is_err());
    }

    #[test]
    fn every_platform_has_a_pinned_build() {
        let mut hw = crate::hardware::detect();
        for (os, arch) in [
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
        ] {
            hw.os = os;
            hw.arch = arch;
            for backend in [Backend::Metal, Backend::Vulkan, Backend::Cpu] {
                hw.backend = backend;
                let asset = runtime_asset(&hw).unwrap();
                assert!(asset.url.contains(LLAMA_BUILD) && asset.sha256.len() == 64);
            }
        }
        hw.os = "freebsd";
        assert!(runtime_asset(&hw).is_none());
    }
}
