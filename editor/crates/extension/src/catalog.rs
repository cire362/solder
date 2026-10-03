//! The two public catalogs: Zed's own, and Open VSX for VS Code extensions.
//! (Microsoft's Marketplace allows only its own products, so it is not used.)

use std::{sync::OnceLock, time::Duration};

use serde_json::Value;

use crate::Origin;

pub const ZED: &str = "https://api.zed.dev";
pub const OPEN_VSX: &str = "https://open-vsx.org";

/// The newest version of Zed's extension API Solder implements. The catalog
/// answers with the newest build of an extension made for it or an older one.
pub const ZED_API: &str = "0.7.0";

/// A search answer is a list of short records; this is far above any of them.
const ANSWER_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub origin: Origin,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub downloads: u64,
    /// What Zed's catalog says it has (`languages`, `themes`, ...). Open VSX
    /// does not say.
    pub provides: Vec<String>,
    /// The archive.
    pub url: String,
    /// Where the archive's SHA-256 is published, if the catalog has one.
    pub sha256_url: Option<String>,
}

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("solder-extensions")
            .enable_all()
            .build()
            .expect("extensions runtime")
    })
}

pub(crate) fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .user_agent(concat!("Solder/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client")
    })
}

/// reqwest's errors name the URL and the cause on separate levels.
pub(crate) fn describe(e: reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}

fn encode(query: &str) -> String {
    let mut out = String::new();
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Extensions matching `query` in the catalog at `base`, in the catalog's
/// own order of relevance. An empty query gives the most downloaded ones.
/// The future can be awaited from any executor.
pub fn search(
    origin: Origin,
    base: &str,
    query: &str,
) -> impl Future<Output = Result<Vec<Entry>, String>> + Send + 'static {
    let base = base.trim_end_matches('/').to_string();
    let browsing = query.trim().is_empty();
    let url = match origin {
        Origin::Zed => format!(
            "{base}/extensions?max_schema_version=1&filter={}",
            encode(query.trim())
        ),
        Origin::VsCode => format!(
            "{base}/api/-/search?size=50&sortBy={}&sortOrder=desc&query={}",
            if browsing {
                "downloadCount"
            } else {
                "relevance"
            },
            encode(query.trim())
        ),
    };
    let handle = runtime().spawn(async move {
        let mut response = client()
            .get(&url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(describe)?;
        if !response.status().is_success() {
            return Err(format!("The catalog answered {}", response.status()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(describe)? {
            if body.len() + chunk.len() > ANSWER_LIMIT {
                return Err("The catalog's answer is too large".into());
            }
            body.extend_from_slice(&chunk);
        }
        let answer: Value =
            serde_json::from_slice(&body).map_err(|_| "The catalog's answer is not JSON")?;
        Ok(match origin {
            Origin::Zed => {
                let mut entries = parse_zed(&base, &answer);
                if browsing {
                    entries.sort_by(|a, b| b.downloads.cmp(&a.downloads));
                }
                entries
            }
            Origin::VsCode => parse_open_vsx(&answer),
        })
    });
    async move { handle.await.map_err(|e| e.to_string())? }
}

/// A name that is safe as a folder: what both catalogs use for ids.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().trim().to_string()
}

fn parse_zed(base: &str, answer: &Value) -> Vec<Entry> {
    answer["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = text(&item["id"]);
            valid_id(&id).then(|| Entry {
                origin: Origin::Zed,
                name: text(&item["name"]),
                version: text(&item["version"]),
                description: text(&item["description"]),
                downloads: item["download_count"].as_u64().unwrap_or(0),
                provides: item["provides"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
                url: format!(
                    "{base}/extensions/{id}/download?min_schema_version=0&max_schema_version=1\
                     &min_wasm_api_version=0.0.1&max_wasm_api_version={ZED_API}"
                ),
                sha256_url: None,
                id,
            })
        })
        .collect()
}

fn parse_open_vsx(answer: &Value) -> Vec<Entry> {
    answer["extensions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = format!("{}.{}", text(&item["namespace"]), text(&item["name"]));
            let url = text(&item["files"]["download"]);
            (valid_id(&id) && !url.is_empty()).then(|| Entry {
                origin: Origin::VsCode,
                name: Some(text(&item["displayName"]))
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| text(&item["name"])),
                version: text(&item["version"]),
                description: text(&item["description"]),
                downloads: item["downloadCount"].as_u64().unwrap_or(0),
                provides: Vec::new(),
                sha256_url: item["files"]["sha256"].as_str().map(str::to_string),
                url,
                id,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Served, block, serve};

    #[test]
    fn reads_zeds_catalog() {
        let answer = r#"{"data":[
            {"id":"tokyo-night","name":"Tokyo Night","version":"1.0.0","description":" A theme ","download_count":5,"provides":["themes"]},
            {"id":"vue","name":"Vue","version":"0.4.0","description":"Vue support.","download_count":675263,"provides":["languages","grammars","language-servers"],"wasm_api_version":"0.7.0"},
            {"id":"../evil","name":"Evil","version":"1","download_count":999999999}
        ]}"#;
        let (base, requests) = serve(vec![(
            "/extensions",
            Served::ok(answer.as_bytes().to_vec()),
        )]);
        let found = block(search(Origin::Zed, &base, "vue js")).unwrap();
        assert_eq!(
            requests.lock().unwrap()[0],
            "/extensions?max_schema_version=1&filter=vue%20js"
        );
        // The catalog's order is kept for a query; an id that is not a plain
        // name is dropped.
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].id, "tokyo-night");
        let found = block(search(Origin::Zed, &base, " ")).unwrap();
        // Without one, the most downloaded come first.
        assert_eq!(found[0].id, "vue");
        assert_eq!(
            found[0].provides,
            ["languages", "grammars", "language-servers"]
        );
        assert_eq!(
            found[0].url,
            format!(
                "{base}/extensions/vue/download?min_schema_version=0&max_schema_version=1&min_wasm_api_version=0.0.1&max_wasm_api_version=0.7.0"
            )
        );
        assert_eq!(found[1].description, "A theme");
        assert_eq!(
            requests.lock().unwrap()[1],
            "/extensions?max_schema_version=1&filter="
        );
    }

    #[test]
    fn reads_open_vsx() {
        let answer = r#"{"offset":0,"totalSize":1,"extensions":[
            {"namespace":"dracula-theme","name":"theme-dracula","displayName":"Dracula Theme Official","version":"2.25.1","description":"A dark theme","downloadCount":427184,
             "files":{"download":"https://open-vsx.org/api/dracula-theme/theme-dracula/2.25.1/file/dracula-theme.theme-dracula-2.25.1.vsix","sha256":"https://open-vsx.org/x.sha256"}},
            {"namespace":"no","name":"download","version":"1","files":{}}
        ]}"#;
        let (base, _) = serve(vec![(
            "/api/-/search",
            Served::ok(answer.as_bytes().to_vec()),
        )]);
        let found = block(search(Origin::VsCode, &base, "dracula")).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "dracula-theme.theme-dracula");
        assert_eq!(found[0].name, "Dracula Theme Official");
        assert_eq!(found[0].downloads, 427184);
        assert_eq!(
            found[0].sha256_url.as_deref(),
            Some("https://open-vsx.org/x.sha256")
        );
    }

    #[test]
    fn a_catalog_that_fails_says_so() {
        let (base, _) = serve(vec![
            ("/extensions", Served::status(503)),
            ("/api/-/search", Served::ok(b"<html>".to_vec())),
        ]);
        assert_eq!(
            block(search(Origin::Zed, &base, "")).unwrap_err(),
            "The catalog answered 503 Service Unavailable"
        );
        assert_eq!(
            block(search(Origin::VsCode, &base, "")).unwrap_err(),
            "The catalog's answer is not JSON"
        );
    }

    #[test]
    fn ids_are_plain_names() {
        assert!(valid_id("vue"));
        assert!(valid_id("Vue.volar"));
        assert!(!valid_id(""));
        assert!(!valid_id(".hidden"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("a b"));
    }
}
