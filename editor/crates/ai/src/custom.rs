//! Models beyond the catalog: any single-file GGUF on Hugging Face, a file
//! on disk, or the models LM Studio has already downloaded. Each is
//! described by its file's header, so it is measured and predicted like a
//! catalog model, but never recommended: its quality is unknown.

use std::path::{Path, PathBuf};

use crate::{
    Model, Source, client, gguf,
    install::{hub_url, model_asset},
};

/// What the user typed: a Hugging Face repository (`org/repo`), one with a
/// quantization (`org/repo:Q8_0`, as llama.cpp's `-hf` takes), a file in it
/// (`org/repo/file.gguf`), or a link to the repository or the file.
#[derive(Debug, PartialEq)]
pub struct HubRef {
    pub repo: String,
    pub file: Option<String>,
    pub quant: Option<String>,
}

pub fn parse_hub(input: &str) -> Option<HubRef> {
    let mut s = input.trim().trim_end_matches('/');
    for prefix in ["https://", "http://"] {
        s = s.strip_prefix(prefix).unwrap_or(s);
    }
    for host in ["huggingface.co/", "hf.co/"] {
        s = s.strip_prefix(host).unwrap_or(s);
    }
    let s = s.split(['?', '#']).next().unwrap_or(s);
    let (path, quant) = match s.rsplit_once(':') {
        Some((p, q)) if !q.contains('/') && !q.is_empty() => (p, Some(q.to_string())),
        _ => (s, None),
    };
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let org = parts.next()?;
    let name = parts.next()?;
    let valid = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    if !valid(org) || !valid(name) {
        return None;
    }
    let rest: Vec<&str> = parts.collect();
    // `blob/<rev>/path` and `resolve/<rev>/path` from copied links.
    let file = match rest.as_slice() {
        [] => None,
        ["blob" | "resolve" | "tree", _rev, path @ ..] if !path.is_empty() => Some(path.join("/")),
        ["blob" | "resolve" | "tree", _rev] => None,
        path => Some(path.join("/")),
    };
    if file.as_deref().is_some_and(|f| !f.ends_with(".gguf")) {
        return None;
    }
    Some(HubRef {
        repo: format!("{org}/{name}"),
        file,
        quant,
    })
}

/// Files that are not a model to run on their own.
fn is_auxiliary(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.starts_with("mmproj")
        || name.starts_with("mtp-")
        || name.starts_with("eagle")
        || name.starts_with("dflash")
        || name.contains("-of-0")
        || path.to_ascii_uppercase().contains("BF16/")
}

/// The file to use from a repository: the one with `quant` in its name,
/// else the usual balance of size and quality (Q4_K_M), else the smallest
/// 4-bit file, else the smallest.
pub fn choose_file(files: &[(String, u64)], quant: Option<&str>) -> Option<String> {
    let usable: Vec<&(String, u64)> = files
        .iter()
        .filter(|(p, _)| p.ends_with(".gguf") && !is_auxiliary(p))
        .collect();
    let has = |p: &str, q: &str| p.to_ascii_uppercase().contains(&q.to_ascii_uppercase());
    if let Some(q) = quant {
        return usable
            .iter()
            .filter(|(p, _)| has(p, q))
            .min_by_key(|(_, size)| *size)
            .map(|(p, _)| p.clone());
    }
    for q in ["Q4_K_M", "UD-Q4_K_XL", "Q4_K_S", "MXFP4", "Q4_0"] {
        if let Some((p, _)) = usable
            .iter()
            .filter(|(p, _)| has(p, q))
            .min_by_key(|(_, s)| *s)
        {
            return Some(p.clone());
        }
    }
    usable
        .iter()
        .filter(|(p, _)| has(p, "Q4"))
        .chain(usable.iter())
        .min_by_key(|(_, s)| *s)
        .map(|(p, _)| p.clone())
}

/// The GGUF files of a repository, with their sizes.
async fn list_files(hub: &str, repo: &str) -> Result<Vec<(String, u64)>, String> {
    let url = format!("{hub}/api/models/{repo}/tree/main?recursive=true");
    let response = client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Could not reach Hugging Face: {e}"))?;
    match response.status().as_u16() {
        200 => {}
        401 | 404 => return Err(format!("No public repository {repo} on Hugging Face")),
        429 => return Err("Hugging Face is limiting requests; try again in a minute".into()),
        code => return Err(format!("Could not list {repo}: HTTP {code}")),
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    let items: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok(items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let path = f["path"].as_str()?;
            path.ends_with(".gguf")
                .then(|| (path.to_string(), f["size"].as_u64().unwrap_or(0)))
        })
        .collect())
}

/// Describes a model on Hugging Face from what the user typed.
pub async fn from_hub(hub: String, input: String) -> Result<Model, String> {
    let r = parse_hub(&input).ok_or_else(|| {
        "Type a Hugging Face repository such as org/model-GGUF, a link to a .gguf file, or a path"
            .to_string()
    })?;
    let file = match r.file {
        Some(file) => file,
        None => {
            let files = list_files(&hub, &r.repo).await?;
            choose_file(&files, r.quant.as_deref()).ok_or_else(|| match &r.quant {
                Some(q) => format!("{} has no single-file {q} model", r.repo),
                None => format!("{} has no single-file GGUF model", r.repo),
            })?
        }
    };
    if is_auxiliary(&file) {
        return Err(format!("{file} is not a model on its own"));
    }
    let asset = model_asset(&hub, &r.repo, &file).await?;
    let info = gguf::read_remote(&hub_url(&hub, &r.repo, &file)).await?;
    Model::from_header(&info, Source::Hub { repo: r.repo, file }, asset.size)
}

/// Describes a GGUF file on disk. Blocks: reads its header.
pub fn from_file(path: &Path) -> Result<Model, String> {
    let path = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if path.extension().is_none_or(|e| e != "gguf") {
        return Err("Pick a .gguf file".into());
    }
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    let info = gguf::read_file(&path)?;
    Model::from_header(&info, Source::File(path), size)
}

/// Where LM Studio keeps the models it downloads.
pub fn lm_studio_dirs() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return Vec::new();
    };
    let home = PathBuf::from(home);
    vec![
        home.join(".lmstudio/models"),
        home.join(".cache/lm-studio/models"),
    ]
}

/// GGUF models under `dirs` (LM Studio nests them as publisher/repo/file).
pub fn scan(dirs: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && depth > 0 {
                walk(&path, depth - 1, out);
            } else if path.extension().is_some_and(|e| e == "gguf")
                && !is_auxiliary(&path.to_string_lossy())
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for dir in dirs {
        walk(dir, 3, &mut out);
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(repo: &str, file: Option<&str>, quant: Option<&str>) -> Option<HubRef> {
        Some(HubRef {
            repo: repo.into(),
            file: file.map(Into::into),
            quant: quant.map(Into::into),
        })
    }

    #[test]
    fn parses_what_people_paste() {
        assert_eq!(
            parse_hub("unsloth/Qwen3.5-9B-GGUF"),
            r("unsloth/Qwen3.5-9B-GGUF", None, None)
        );
        assert_eq!(
            parse_hub(" unsloth/Qwen3.5-9B-GGUF:Q8_0 "),
            r("unsloth/Qwen3.5-9B-GGUF", None, Some("Q8_0"))
        );
        assert_eq!(
            parse_hub(
                "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/blob/main/Qwen3.5-9B-Q4_0.gguf"
            ),
            r(
                "unsloth/Qwen3.5-9B-GGUF",
                Some("Qwen3.5-9B-Q4_0.gguf"),
                None
            )
        );
        assert_eq!(
            parse_hub("hf.co/org/repo/resolve/main/sub/m.gguf?download=true"),
            r("org/repo", Some("sub/m.gguf"), None)
        );
        assert_eq!(
            parse_hub("https://huggingface.co/org/repo/tree/main"),
            r("org/repo", None, None)
        );
        assert_eq!(
            parse_hub("org/repo/m.gguf"),
            r("org/repo", Some("m.gguf"), None)
        );
        assert_eq!(parse_hub("org/repo/README.md"), None);
        assert_eq!(parse_hub("just-a-name"), None);
        assert_eq!(parse_hub("org/re po"), None);
    }

    #[test]
    fn chooses_a_sensible_file() {
        let files: Vec<(String, u64)> = [
            ("m-Q8_0.gguf", 900),
            ("m-Q4_K_M.gguf", 500),
            ("m-Q4_0.gguf", 480),
            ("m-Q2_K.gguf", 300),
            ("mmproj-m-F16.gguf", 100),
            ("BF16/m-BF16-00001-of-00002.gguf", 2000),
            ("README.md", 1),
        ]
        .iter()
        .map(|(p, s)| (p.to_string(), *s))
        .collect();
        assert_eq!(choose_file(&files, None).as_deref(), Some("m-Q4_K_M.gguf"));
        assert_eq!(
            choose_file(&files, Some("q8_0")).as_deref(),
            Some("m-Q8_0.gguf")
        );
        assert_eq!(choose_file(&files, Some("Q6_K")), None);
        let odd: Vec<(String, u64)> = vec![("a-IQ3.gguf".into(), 9), ("a-F16.gguf".into(), 20)];
        assert_eq!(choose_file(&odd, None).as_deref(), Some("a-IQ3.gguf"));
        let only_aux: Vec<(String, u64)> = vec![("mmproj.gguf".into(), 1)];
        assert_eq!(choose_file(&only_aux, None), None);
    }

    #[test]
    fn reads_files_and_finds_lm_studio_models() {
        use crate::gguf::tests::{MetaValue::*, header};
        let root = std::env::temp_dir().join(format!("solder-custom-{}", std::process::id()));
        let nested = root.join("lmstudio-community/Tiny-GGUF");
        std::fs::create_dir_all(&nested).unwrap();
        let data = header(
            &[
                ("general.architecture", Str("llama")),
                ("llama.block_count", U32(1)),
                ("llama.embedding_length", U32(8)),
                ("llama.attention.head_count", U32(2)),
            ],
            &[("output.weight", &[8, 8])],
        );
        std::fs::write(nested.join("Tiny-Q4_K_M.gguf"), &data).unwrap();
        std::fs::write(nested.join("mmproj-Tiny.gguf"), &data).unwrap();
        std::fs::write(nested.join("notes.txt"), "x").unwrap();
        let found = scan(std::slice::from_ref(&root));
        assert_eq!(found.len(), 1);
        let model = from_file(&found[0]).unwrap();
        assert_eq!(model.name, "Tiny-Q4_K_M");
        assert!(model.id.starts_with("file:") && model.is_custom());
        assert_eq!(model.size, data.len() as u64);
        assert!(from_file(&nested.join("notes.txt")).is_err());
        assert!(scan(&[root.join("missing")]).is_empty());
    }

    #[test]
    fn describes_a_hub_model_from_its_header() {
        use crate::gguf::tests::{MetaValue::*, header};
        use std::io::{BufRead, BufReader, Write};
        let data = header(
            &[
                ("general.architecture", Str("llama")),
                ("llama.block_count", U32(2)),
                ("llama.embedding_length", U32(8)),
                ("llama.attention.head_count", U32(2)),
            ],
            &[("output.weight", &[1000, 1000])],
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let hub = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                let body: Vec<u8> = if first.contains("/api/models/org/repo/tree") {
                    br#"[{"path":"m-Q8_0.gguf","size":9000},{"path":"m-Q4_K_M.gguf","size":5000},{"path":"README.md","size":1}]"#.to_vec()
                } else if first.starts_with("HEAD") {
                    let sha = "a".repeat(64);
                    let _ = write!(
                        stream,
                        "HTTP/1.1 302 Found\r\nX-Linked-Size: 5000000\r\nX-Linked-Etag: \"{sha}\"\r\nContent-Length: 0\r\n\r\n"
                    );
                    continue;
                } else if first.contains("/org/repo/resolve/main/m-Q4_K_M.gguf") {
                    data.clone()
                } else {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"
                    );
                    continue;
                };
                let status = if first.contains("resolve") {
                    "206 Partial Content"
                } else {
                    "200 OK"
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        let model = crate::runtime()
            .block_on(from_hub(hub.clone(), "org/repo".into()))
            .unwrap();
        assert_eq!(model.id, "hf:org/repo/m-Q4_K_M.gguf");
        assert_eq!(model.size, 5_000_000);
        assert_eq!(model.params, 0.001);
        let err = crate::runtime()
            .block_on(from_hub(hub.clone(), "org/missing".into()))
            .unwrap_err();
        assert!(err.contains("No public repository"), "{err}");
        let err = crate::runtime()
            .block_on(from_hub(hub, "org/repo:Q6_K".into()))
            .unwrap_err();
        assert!(err.contains("no single-file Q6_K"), "{err}");
    }
}
