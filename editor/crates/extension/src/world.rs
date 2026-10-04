//! The real world an extension's code works in: npm, GitHub's releases,
//! downloads and the user's PATH.
//!
//! Everything here blocks. npm installs with `--ignore-scripts`, so a
//! package's own install hooks do not run; archives are unpacked with the
//! system's `tar`, `unzip` and `gzip`.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    sync::OnceLock,
    time::Duration,
};

use serde_json::Value;

use crate::{
    catalog::{client, describe, runtime},
    host::{Command, FileKind, HttpRequest, HttpResponse, Output, Release, Status, World},
};

pub const GITHUB: &str = "https://api.github.com";

/// The largest file an extension may download: language servers with their
/// runtimes reach hundreds of megabytes.
const DOWNLOAD_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
/// The largest answer `fetch` hands to an extension in memory.
const FETCH_LIMIT: usize = 64 * 1024 * 1024;

/// Hears what an extension says about a server it is getting ready.
type OnStatus = Box<dyn Fn(&str, Status) + Send + Sync>;

pub struct System {
    /// The environment the user's tools start with: their shell's, where
    /// PATH has Node and whatever else they installed.
    env: Vec<(String, String)>,
    /// The npm to run; a name looked up on PATH, or a path.
    pub npm: String,
    /// Where GitHub's API is; tests point it at a local server.
    pub github: String,
    on_status: OnStatus,
}

impl System {
    pub fn new(
        env: Vec<(String, String)>,
        on_status: impl Fn(&str, Status) + Send + Sync + 'static,
    ) -> Self {
        Self {
            env,
            npm: "npm".into(),
            github: GITHUB.into(),
            on_status: Box::new(on_status),
        }
    }

    fn path(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.env
            .iter()
            .find(|(name, _)| name == "PATH")
            .into_iter()
            .flat_map(|(_, path)| std::env::split_paths(path))
    }

    /// Runs a program with the user's environment and gives its output, or
    /// what it wrote to stderr if it failed.
    fn output(&self, program: &str, args: &[&str], dir: Option<&Path>) -> Result<String, String> {
        let program = self.which(program).unwrap_or_else(|| program.to_string());
        let mut process = Process::new(&program);
        process
            .args(args)
            .env_clear()
            .envs(self.env.iter().cloned());
        process.stdin(Stdio::null());
        if let Some(dir) = dir {
            process.current_dir(dir);
        }
        let out = process
            .output()
            .map_err(|e| format!("Could not run {program}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let said = String::from_utf8_lossy(&out.stderr);
            let said = said.trim();
            // The last line is the error; what precedes it is npm's log.
            Err(said.lines().last().unwrap_or("it failed").to_string())
        }
    }
}

fn is_program(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    path.is_file()
}

/// A client that answers a redirect as it is.
fn direct() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .redirect_policy(reqwest::redirect::Policy::none())
            .user_agent(concat!("Solder/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client")
    })
}

fn run(command: &mut Process) -> Result<(), String> {
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

impl World for System {
    fn node(&self) -> Result<String, String> {
        self.which("node")
            .ok_or_else(|| "Node.js was not found, and this language server needs it".to_string())
    }

    fn npm_latest(&self, package: &str) -> Result<String, String> {
        let version = self.output(&self.npm, &["view", package, "version"], None)?;
        if version.is_empty() {
            return Err(format!("npm knows no version of {package}"));
        }
        Ok(version)
    }

    fn npm_install(&self, dir: &Path, package: &str, version: &str) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let prefix = dir.to_string_lossy();
        self.output(
            &self.npm,
            &[
                "install",
                "--prefix",
                &prefix,
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                &format!("{package}@{version}"),
            ],
            Some(dir),
        )
        .map(|_| ())
    }

    fn release(&self, repo: &str, tag: Option<&str>, pre_release: bool) -> Result<Release, String> {
        let plain = |text: &str| {
            !text.is_empty()
                && text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
        };
        let owner_and_name: Vec<&str> = repo.split('/').collect();
        if owner_and_name.len() != 2 || !owner_and_name.iter().all(|part| plain(part)) {
            return Err(format!("{repo} is not a GitHub repository"));
        }
        let url = match tag {
            Some(tag) if plain(tag) => format!("{}/repos/{repo}/releases/tags/{tag}", self.github),
            Some(tag) => return Err(format!("{tag} is not a release tag")),
            None => format!("{}/repos/{repo}/releases?per_page=30", self.github),
        };
        let answer: Value = runtime().block_on(async {
            let response = client()
                .get(&url)
                .header("Accept", "application/vnd.github+json")
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(describe)?;
            if !response.status().is_success() {
                return Err(format!("GitHub answered {} for {repo}", response.status()));
            }
            let body = response.bytes().await.map_err(describe)?;
            serde_json::from_slice(&body)
                .map_err(|_| format!("GitHub's answer for {repo} is not JSON"))
        })?;
        let release = match &answer {
            Value::Array(releases) => releases
                .iter()
                .find(|r| r["draft"] != true && (pre_release || r["prerelease"] != true))
                .ok_or_else(|| format!("{repo} has no release"))?,
            release => release,
        };
        Ok(Release {
            version: release["tag_name"]
                .as_str()
                .ok_or_else(|| format!("{repo} has no release"))?
                .to_string(),
            assets: release["assets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|asset| {
                    Some((
                        asset["name"].as_str()?.to_string(),
                        asset["browser_download_url"].as_str()?.to_string(),
                    ))
                })
                .collect(),
        })
    }

    fn download(&self, url: &str, dest: &Path, kind: FileKind) -> Result<(), String> {
        let parent = dest.parent().ok_or("No folder to download into")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let mut name = dest.file_name().unwrap_or_default().to_os_string();
        name.push(".download");
        let fetched = parent.join(name);
        let result = (|| {
            let mut file = std::fs::File::create(&fetched).map_err(|e| e.to_string())?;
            runtime().block_on(async {
                let mut response = client().get(url).send().await.map_err(describe)?;
                if !response.status().is_success() {
                    return Err(format!("The download answered {}", response.status()));
                }
                let mut done = 0u64;
                while let Some(chunk) = response.chunk().await.map_err(describe)? {
                    done += chunk.len() as u64;
                    if done > DOWNLOAD_LIMIT {
                        return Err("The download is too large".to_string());
                    }
                    file.write_all(&chunk).map_err(|e| e.to_string())?;
                }
                file.flush().map_err(|e| e.to_string())
            })?;
            match kind {
                FileKind::Uncompressed => {
                    std::fs::rename(&fetched, dest).map_err(|e| e.to_string())
                }
                FileKind::Gzip => {
                    let out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
                    run(Process::new("gzip").arg("-dc").arg(&fetched).stdout(out))
                }
                FileKind::GzipTar => {
                    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
                    run(Process::new("tar")
                        .arg("-xzf")
                        .arg(&fetched)
                        .arg("-C")
                        .arg(dest))
                }
                FileKind::Zip => {
                    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
                    let unzip = run(Process::new("unzip")
                        .arg("-q")
                        .arg("-o")
                        .arg(&fetched)
                        .arg("-d")
                        .arg(dest));
                    // Where there is no unzip, the system's tar may read zip.
                    unzip.or_else(|e| {
                        run(Process::new("tar")
                            .arg("-xf")
                            .arg(&fetched)
                            .arg("-C")
                            .arg(dest))
                        .map_err(|_| e)
                    })
                }
            }
        })();
        let _ = std::fs::remove_file(&fetched);
        result
    }

    fn fetch(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| format!("{} is not an HTTP method", request.method))?;
        // A limit between none and all is followed like all: the client
        // stops at ten on its own.
        let client = if request.redirects == Some(0) {
            direct()
        } else {
            client()
        };
        runtime().block_on(async {
            let mut builder = client
                .request(method, &request.url)
                .timeout(Duration::from_secs(120));
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let mut response = builder.send().await.map_err(describe)?;
            let status = response.status();
            if status.is_client_error() || status.is_server_error() {
                return Err(format!("{} answered {status}", request.url));
            }
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.to_string(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(describe)? {
                if body.len() + chunk.len() > FETCH_LIMIT {
                    return Err(format!("The answer of {} is too large", request.url));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(HttpResponse { headers, body })
        })
    }

    fn run(&self, command: &Command) -> Result<Output, String> {
        let program = self
            .which(&command.command)
            .unwrap_or_else(|| command.command.clone());
        let out = Process::new(&program)
            .args(&command.args)
            .env_clear()
            .envs(self.env.iter().cloned())
            .envs(command.env.iter().cloned())
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("Could not run {program}: {e}"))?;
        Ok(Output {
            status: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    fn which(&self, binary: &str) -> Option<String> {
        if binary.contains('/') {
            return is_program(Path::new(binary)).then(|| binary.to_string());
        }
        self.path()
            .map(|dir| dir.join(binary))
            .find(|path| is_program(path))
            .map(|path| path.to_string_lossy().into_owned())
    }

    fn env(&self) -> Vec<(String, String)> {
        self.env.clone()
    }

    fn status(&self, server: &str, status: Status) {
        (self.on_status)(server, status);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::testing::{Served, scratch, serve, tar, write, zip};

    fn system(env: &[(&str, &str)]) -> System {
        System::new(
            env.iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            |_, _| {},
        )
    }

    fn executable(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        write(path, text);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn programs_are_found_on_the_users_path() {
        let dir = scratch("world-which");
        executable(&dir.join("bin/tool"), "#!/bin/sh\n");
        write(&dir.join("first/tool"), "not executable");
        let path = format!(
            "{}:{}",
            dir.join("first").display(),
            dir.join("bin").display()
        );
        let world = system(&[("PATH", &path)]);
        let found = dir.join("bin/tool").to_string_lossy().into_owned();
        assert_eq!(world.which("tool"), Some(found.clone()));
        assert_eq!(world.which(&found), Some(found));
        assert_eq!(world.which("absent"), None);
        assert_eq!(world.which("/no/such/tool"), None);
        assert_eq!(system(&[]).which("tool"), None);
        assert!(
            world
                .node()
                .unwrap_err()
                .starts_with("Node.js was not found")
        );
    }

    #[test]
    fn npm_is_asked_and_installs_without_running_scripts() {
        let dir = scratch("world-npm");
        // An npm that records how it was called and answers like the real one.
        let log = dir.join("calls.txt");
        executable(
            &dir.join("bin/npm"),
            &format!(
                "#!/bin/sh\necho \"$PWD: $*\" >> {log}\ncase \"$1\" in\n  view) [ \"$2\" = absent ] && {{ echo 'npm ERR! log' >&2; echo 'npm error 404 Not Found' >&2; exit 1; }}; echo ' 4.10.0 ' ;;\n  install) mkdir -p \"$3/node_modules\" ;;\nesac\n",
                log = log.display()
            ),
        );
        let path = format!("{}:/usr/bin:/bin", dir.join("bin").display());
        let world = system(&[("PATH", &path)]);
        assert_eq!(world.npm_latest("some-server").unwrap(), "4.10.0");
        assert_eq!(
            world.npm_latest("absent").unwrap_err(),
            "npm error 404 Not Found"
        );
        let work = dir.join("work").join("html");
        world.npm_install(&work, "@scope/server", "4.10.0").unwrap();
        assert!(work.join("node_modules").is_dir());
        let calls = std::fs::read_to_string(&log).unwrap();
        let work = work.canonicalize().unwrap();
        assert!(calls.contains("view some-server version"), "{calls}");
        assert!(
            calls.contains(&format!(
                "{}: install --prefix {} --ignore-scripts --no-audit --no-fund @scope/server@4.10.0",
                work.display(),
                dir.join("work/html").display()
            )),
            "{calls}"
        );
        // No npm at all.
        let error = system(&[("PATH", "/nowhere")]).npm_latest("x").unwrap_err();
        assert!(error.starts_with("Could not run npm"), "{error}");
    }

    #[test]
    fn releases_come_from_githubs_api() {
        let list = r#"[
            {"tag_name":"v2.0.0-draft","draft":true,"prerelease":false,"assets":[]},
            {"tag_name":"v2.0.0-rc1","draft":false,"prerelease":true,"assets":[]},
            {"tag_name":"v1.4.0","draft":false,"prerelease":false,"assets":[
                {"name":"tool-aarch64.gz","browser_download_url":"https://example.com/tool-aarch64.gz"},
                {"name":"broken"}
            ]}
        ]"#;
        let tagged = r#"{"tag_name":"v1.0.0","assets":[]}"#;
        let (base, requests) = serve(vec![
            (
                "/repos/acme/tool/releases",
                Served::ok(list.as_bytes().to_vec()),
            ),
            (
                "/repos/acme/tool/releases/tags/v1.0.0",
                Served::ok(tagged.as_bytes().to_vec()),
            ),
            ("/repos/acme/empty/releases", Served::ok(b"[]".to_vec())),
        ]);
        let mut world = system(&[]);
        world.github = base;
        let stable = world.release("acme/tool", None, false).unwrap();
        assert_eq!(stable.version, "v1.4.0");
        assert_eq!(
            stable.assets,
            [(
                "tool-aarch64.gz".to_string(),
                "https://example.com/tool-aarch64.gz".to_string()
            )]
        );
        assert_eq!(
            world.release("acme/tool", None, true).unwrap().version,
            "v2.0.0-rc1"
        );
        assert_eq!(
            world
                .release("acme/tool", Some("v1.0.0"), true)
                .unwrap()
                .version,
            "v1.0.0"
        );
        assert_eq!(
            world.release("acme/empty", None, false).unwrap_err(),
            "acme/empty has no release"
        );
        assert_eq!(
            world.release("acme/absent", None, false).unwrap_err(),
            "GitHub answered 404 Not Found for acme/absent"
        );
        // Names that would change the address are refused before any request.
        let before = requests.lock().unwrap().len();
        assert!(world.release("acme/tool/../../user", None, false).is_err());
        assert!(world.release("acme/tool?x=1", None, false).is_err());
        assert!(world.release("acme/tool", Some("../v1"), false).is_err());
        assert_eq!(requests.lock().unwrap().len(), before);
    }

    #[test]
    fn downloads_are_unpacked_by_kind() {
        let dir = scratch("world-download");
        write(&dir.join("tree/bin/server"), "#!/bin/sh\necho server\n");
        let plain = b"plain file".to_vec();
        let gz = {
            let file = dir.join("one.txt");
            write(&file, "gzipped file");
            let out = Process::new("gzip").arg("-c").arg(&file).output().unwrap();
            assert!(out.status.success());
            out.stdout
        };
        let mut routes = vec![
            ("/plain", Served::ok(plain)),
            ("/one.gz", Served::ok(gz)),
            ("/tree.tgz", Served::ok(tar(&dir.join("tree")))),
            ("/redirect", Served::redirect("/plain")),
        ];
        let zipped = zip(&dir.join("tree"), "bin");
        if let Some(zipped) = zipped.clone() {
            routes.push(("/tree.zip", Served::ok(zipped)));
        }
        let (base, _) = serve(routes);
        let world = system(&[]);
        let out = dir.join("out");
        let get = |path: &str, name: &str, kind| {
            world.download(&format!("{base}{path}"), &out.join(name), kind)
        };

        get("/redirect", "a/plain.txt", FileKind::Uncompressed).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("a/plain.txt")).unwrap(),
            "plain file"
        );
        get("/one.gz", "one.txt", FileKind::Gzip).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("one.txt")).unwrap(),
            "gzipped file"
        );
        get("/tree.tgz", "tree", FileKind::GzipTar).unwrap();
        assert!(out.join("tree/bin/server").is_file());
        if zipped.is_some() {
            get("/tree.zip", "zipped", FileKind::Zip).unwrap();
            assert!(out.join("zipped/bin/server").is_file());
        } else {
            eprintln!("skipped the zip: no python3 to build one");
        }
        assert_eq!(
            get("/absent", "x", FileKind::Uncompressed).unwrap_err(),
            "The download answered 404 Not Found"
        );
        assert!(
            get("/plain", "bad", FileKind::GzipTar)
                .unwrap_err()
                .starts_with("Could not unpack")
        );
        // Nothing half-downloaded is left behind.
        let left: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".download"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn fetch_and_run_and_status() {
        let (base, _) = serve(vec![
            ("/data", Served::ok(b"{\"ok\":true}".to_vec())),
            ("/moved", Served::redirect("/data")),
            ("/gone", Served::status(410)),
        ]);
        let said = Arc::new(Mutex::new(Vec::new()));
        let heard = said.clone();
        let world = System::new(
            vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("GREETING".into(), "hi".into()),
            ],
            move |server, status| heard.lock().unwrap().push((server.to_string(), status)),
        );
        let request = |path: &str, redirects| HttpRequest {
            method: "GET",
            url: format!("{base}{path}"),
            headers: vec![("Accept".into(), "application/json".into())],
            body: None,
            redirects,
        };
        assert_eq!(
            world.fetch(request("/data", None)).unwrap().body,
            b"{\"ok\":true}"
        );
        assert_eq!(
            world.fetch(request("/moved", None)).unwrap().body,
            b"{\"ok\":true}"
        );
        // Told not to follow, it hands back the redirect itself.
        let redirect = world.fetch(request("/moved", Some(0))).unwrap();
        assert!(redirect.body.is_empty());
        assert!(
            redirect
                .headers
                .iter()
                .any(|(name, value)| name == "location" && value == "/data")
        );
        assert!(
            world
                .fetch(request("/gone", None))
                .unwrap_err()
                .ends_with("answered 410 Gone")
        );

        let output = world
            .run(&Command {
                command: "sh".into(),
                args: vec!["-c".into(), "echo $GREETING $EXTRA; exit 3".into()],
                env: vec![("EXTRA".into(), "there".into())],
            })
            .unwrap();
        assert_eq!(output.status, Some(3));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "hi there\n");

        world.status("demo-ls", Status::Downloading);
        assert_eq!(
            *said.lock().unwrap(),
            [("demo-ls".to_string(), Status::Downloading)]
        );
    }
}
