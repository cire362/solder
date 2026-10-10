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
        // Asked for this machine: an extension with a build for each
        // platform answers with ours, and one with a single build with it.
        Origin::VsCode => format!(
            "{base}/api/-/search?size=50&sortBy={}&sortOrder=desc&targetPlatform={}&query={}",
            if browsing {
                "downloadCount"
            } else {
                "relevance"
            },
            target(),
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
                    entries.sort_by_key(|entry| std::cmp::Reverse(entry.downloads));
                }
                entries
            }
            Origin::VsCode => parse_open_vsx(&answer),
        })
    });
    async move { handle.await.map_err(|e| e.to_string())? }
}

/// This machine, as VS Code's extensions name the platform a build is
/// for. One that has a build for every platform has none called this
/// but one called `universal`, which the catalog answers with then.
pub fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", _) => "darwin-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "arm") => "linux-armhf",
        ("linux", _) => "linux-x64",
        ("windows", "aarch64") => "win32-arm64",
        ("windows", _) => "win32-x64",
        _ => "universal",
    }
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
        .filter_map(open_vsx_entry)
        .collect()
}

/// One extension as Open VSX describes it, in a search or on its own.
fn open_vsx_entry(item: &Value) -> Option<Entry> {
    let id = format!("{}.{}", text(&item["namespace"]), text(&item["name"]));
    // Where it lists a download for each platform, ours; else the one it
    // answered with.
    let url = Some(text(&item["downloads"][target()]))
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| text(&item["files"]["download"]));
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
}

async fn json(url: &str) -> Result<Value, String> {
    let mut response = client()
        .get(url)
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
    serde_json::from_slice(&body).map_err(|_| "The catalog's answer is not JSON".to_string())
}

/// A pinned Open VSX version, so a compatibility check is reproducible.
pub fn version(
    base: &str,
    id: &str,
    version: &str,
) -> impl Future<Output = Result<Entry, String>> + Send + 'static {
    let (base, id, version) = (
        base.trim_end_matches('/').to_string(),
        id.to_string(),
        version.to_string(),
    );
    let handle = runtime().spawn(async move {
        if !valid_id(&id) || !valid_id(&version) {
            return Err("Invalid extension id or version".into());
        }
        let (namespace, name) = id.split_once('.').ok_or("An extension needs a namespace")?;
        let answer = json(&format!("{base}/api/{namespace}/{name}/{version}")).await?;
        let entry = open_vsx_entry(&answer).ok_or("This version has no download")?;
        if entry.version != version || !entry.id.eq_ignore_ascii_case(&id) {
            return Err("The catalog answered with a different extension version".into());
        }
        Ok(entry)
    });
    async move { handle.await.map_err(|e| e.to_string())? }
}

/// The newest version the catalog at `base` has of each of `ids`, to compare
/// with what is installed. Zed's answers for all in one request; Open VSX
/// is asked for each, and one it does not have is left out. The future can
/// be awaited from any executor.
pub fn latest(
    origin: Origin,
    base: &str,
    ids: Vec<String>,
) -> impl Future<Output = Result<Vec<Entry>, String>> + Send + 'static {
    let base = base.trim_end_matches('/').to_string();
    let ids: Vec<String> = ids.into_iter().filter(|id| valid_id(id)).collect();
    let handle = runtime().spawn(async move {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        match origin {
            Origin::Zed => {
                let url = format!(
                    "{base}/extensions/updates?min_schema_version=0&max_schema_version=1\
                     &min_wasm_api_version=0.0.1&max_wasm_api_version={ZED_API}&ids={}",
                    ids.join(",")
                );
                Ok(parse_zed(&base, &json(&url).await?))
            }
            Origin::VsCode => {
                let mut entries = Vec::new();
                let mut failure = None;
                for id in &ids {
                    let Some((namespace, name)) = id.split_once('.') else {
                        continue;
                    };
                    // The build for this machine, if it has one of its
                    // own; else the one for all of them.
                    let ours = format!("{base}/api/{namespace}/{name}/{}", target());
                    let answer = match json(&ours).await {
                        Ok(answer) => Ok(answer),
                        Err(_) => json(&format!("{base}/api/{namespace}/{name}")).await,
                    };
                    match answer {
                        Ok(answer) => entries.extend(open_vsx_entry(&answer)),
                        Err(error) => failure = Some(error),
                    }
                }
                // Nothing at all came back: say why. Otherwise an extension
                // the catalog no longer has is no reason to hide the rest.
                match (entries.is_empty(), failure) {
                    (true, Some(error)) => Err(error),
                    _ => Ok(entries),
                }
            }
        }
    });
    async move { handle.await.map_err(|e| e.to_string())? }
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
    fn an_extension_with_a_build_for_each_platform_gives_ours() {
        // Asked for by name, the catalog has a build for this machine.
        let ours = format!(
            r#"{{"namespace":"Acme","name":"native","version":"2.0.0","targetPlatform":"{}",
                "files":{{"download":"https://open-vsx.org/native-ours.vsix"}}}}"#,
            target()
        );
        let any = r#"{"namespace":"Acme","name":"native","version":"2.0.0",
            "files":{"download":"https://open-vsx.org/native-other.vsix"}}"#;
        // One with a single build is not known under this machine's name.
        let plain = r#"{"namespace":"Acme","name":"plain","version":"1.0.0",
            "files":{"download":"https://open-vsx.org/plain.vsix"}}"#;
        let (base, requests) = serve(vec![
            (
                Box::leak(format!("/api/Acme/native/{}", target()).into_boxed_str()),
                Served::ok(ours.into_bytes()),
            ),
            ("/api/Acme/native", Served::ok(any.as_bytes().to_vec())),
            (
                Box::leak(format!("/api/Acme/plain/{}", target()).into_boxed_str()),
                Served::status(404),
            ),
            ("/api/Acme/plain", Served::ok(plain.as_bytes().to_vec())),
        ]);
        let found = block(latest(
            Origin::VsCode,
            &base,
            vec!["Acme.native".into(), "Acme.plain".into()],
        ))
        .unwrap();
        let urls: Vec<&str> = found.iter().map(|entry| entry.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://open-vsx.org/native-ours.vsix",
                "https://open-vsx.org/plain.vsix"
            ]
        );
        assert!(
            requests
                .lock()
                .unwrap()
                .contains(&"/api/Acme/plain".to_string())
        );

        // A search says which machine it is for, and an answer that lists
        // a download for each platform is read for ours.
        let listed = format!(
            r#"{{"extensions":[{{"namespace":"Acme","name":"native","version":"2.0.0",
                "files":{{"download":"https://open-vsx.org/first.vsix"}},
                "downloads":{{"{}":"https://open-vsx.org/listed-ours.vsix","web":"https://open-vsx.org/web.vsix"}}}}]}}"#,
            target()
        );
        let (base, requests) = serve(vec![("/api/-/search", Served::ok(listed.into_bytes()))]);
        let found = block(search(Origin::VsCode, &base, "native")).unwrap();
        assert_eq!(found[0].url, "https://open-vsx.org/listed-ours.vsix");
        assert!(
            requests.lock().unwrap()[0].contains(&format!("targetPlatform={}", target())),
            "{:?}",
            requests.lock().unwrap()
        );
        assert!(
            ["darwin", "linux", "win32", "universal"]
                .iter()
                .any(|os| target().starts_with(os))
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
    fn asks_for_the_newest_versions_of_what_is_installed() {
        let zed = r#"{"data":[{"id":"vue","name":"Vue","version":"0.5.0","download_count":1}]}"#;
        let volar = r#"{"namespace":"Vue","name":"volar","version":"3.4.0","displayName":"Vue (Official)",
            "files":{"download":"https://open-vsx.org/volar.vsix","sha256":"https://open-vsx.org/volar.sha256"}}"#;
        let (base, requests) = serve(vec![
            ("/extensions/updates", Served::ok(zed.as_bytes().to_vec())),
            ("/api/Vue/volar", Served::ok(volar.as_bytes().to_vec())),
        ]);
        let found = block(latest(
            Origin::Zed,
            &base,
            vec!["vue".into(), "html".into(), "../x".into()],
        ));
        assert_eq!(found.unwrap()[0].version, "0.5.0");
        assert_eq!(
            requests.lock().unwrap()[0],
            "/extensions/updates?min_schema_version=0&max_schema_version=1&min_wasm_api_version=0.0.1&max_wasm_api_version=0.7.0&ids=vue,html"
        );
        // Open VSX is asked for each; one it no longer has is left out.
        let found = block(latest(
            Origin::VsCode,
            &base,
            vec!["Vue.volar".into(), "gone.extension".into()],
        ))
        .unwrap();
        assert_eq!(found.len(), 1);
        // It was asked for the build for this machine first.
        assert!(
            requests
                .lock()
                .unwrap()
                .contains(&format!("/api/Vue/volar/{}", target()))
        );
        assert_eq!(
            (found[0].id.as_str(), found[0].version.as_str()),
            ("Vue.volar", "3.4.0")
        );
        // None of them answered: the reason is given.
        assert_eq!(
            block(latest(Origin::VsCode, &base, vec!["gone.extension".into()])).unwrap_err(),
            "The catalog answered 404 Not Found"
        );
        // Nothing installed, nothing asked.
        let before = requests.lock().unwrap().len();
        assert!(
            block(latest(Origin::Zed, &base, Vec::new()))
                .unwrap()
                .is_empty()
        );
        assert_eq!(requests.lock().unwrap().len(), before);
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
