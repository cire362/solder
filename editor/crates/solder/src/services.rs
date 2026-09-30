//! Finding the services a project runs, and reading what they print.
//!
//! Detection reads manifests the way a developer would: `dev` scripts in
//! `package.json`, `main` packages in Go modules, binaries in Cargo crates,
//! Django and FastAPI entry points, and services in compose files. A
//! `.solder/services.json` file adds or overrides entries. Everything here
//! touches the disk: callers run it on the background executor.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use regex::Regex;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceKind {
    Node,
    Go,
    Rust,
    Python,
    Compose,
    Custom,
}

impl ServiceKind {
    pub fn label(self) -> &'static str {
        match self {
            ServiceKind::Node => "node",
            ServiceKind::Go => "go",
            ServiceKind::Rust => "rust",
            ServiceKind::Python => "python",
            ServiceKind::Compose => "compose",
            ServiceKind::Custom => "custom",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Unique within the project.
    pub name: String,
    pub kind: ServiceKind,
    /// Shell command, run through the user's login shell so version managers
    /// (nvm, asdf, pyenv) resolve the same way they do in a terminal.
    pub command: String,
    /// Relative to the project root.
    pub dir: PathBuf,
    pub env: BTreeMap<String, String>,
    /// Ports known before starting (compose mappings, custom config).
    pub ports: Vec<u16>,
}

/// Directories never searched for manifests.
fn skip_dir(name: &str) -> bool {
    crate::project::is_excluded_dir(name)
        || matches!(
            name,
            "vendor" | "venv" | ".venv" | "__pycache__" | "build" | "out"
        )
        || name.starts_with('.')
}

/// Manifests up to `depth` folders below the root, in a stable order.
fn find_files(root: &Path, names: &[&str], depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), 0)];
    while let Some((dir, level)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if level < depth && !skip_dir(&name) {
                    dirs.push((entry.path(), level + 1));
                }
            } else if names.contains(&name.as_str()) {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    found
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

fn dir_name(root: &Path, dir: &Path) -> String {
    dir.file_name()
        .or_else(|| root.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "app".into())
}

/// The package manager a folder uses, judged by the nearest lockfile.
fn node_manager(dir: &Path, root: &Path) -> &'static str {
    for d in dir.ancestors() {
        if d.join("pnpm-lock.yaml").exists() {
            return "pnpm";
        }
        if d.join("yarn.lock").exists() {
            return "yarn";
        }
        if d.join("bun.lockb").exists() || d.join("bun.lock").exists() {
            return "bun";
        }
        if d.join("package-lock.json").exists() || d == root {
            break;
        }
    }
    "npm"
}

fn detect_node(root: &Path, out: &mut Vec<ServiceSpec>) {
    #[derive(Deserialize)]
    struct Package {
        name: Option<String>,
        #[serde(default)]
        scripts: BTreeMap<String, String>,
    }
    for manifest in find_files(root, &["package.json"], 3) {
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(package) = serde_json::from_str::<Package>(&text) else {
            continue;
        };
        let Some(script) = ["dev", "start", "serve"]
            .into_iter()
            .find(|s| package.scripts.contains_key(*s))
        else {
            continue;
        };
        let dir = manifest.parent().unwrap_or(root);
        let manager = node_manager(dir, root);
        let command = match manager {
            "yarn" => format!("yarn {script}"),
            m => format!("{m} run {script}"),
        };
        let name = package
            .name
            .map(|n| n.rsplit('/').next().unwrap_or(&n).to_string())
            .unwrap_or_else(|| dir_name(root, dir));
        out.push(ServiceSpec {
            name,
            kind: ServiceKind::Node,
            command,
            dir: relative(root, dir),
            env: BTreeMap::new(),
            ports: Vec::new(),
        });
    }
}

fn detect_go(root: &Path, out: &mut Vec<ServiceSpec>) {
    for manifest in find_files(root, &["go.mod"], 3) {
        let dir = manifest.parent().unwrap_or(root);
        let is_main = |d: &Path| {
            std::fs::read_to_string(d.join("main.go"))
                .is_ok_and(|t| t.lines().any(|l| l.trim() == "package main"))
        };
        let mut targets: Vec<(String, String)> = Vec::new();
        if is_main(dir) {
            targets.push((dir_name(root, dir), ".".into()));
        }
        if let Ok(entries) = std::fs::read_dir(dir.join("cmd")) {
            let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                if is_main(&entry.path()) {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    targets.push((name.clone(), format!("./cmd/{name}")));
                }
            }
        }
        for (name, target) in targets {
            out.push(ServiceSpec {
                name,
                kind: ServiceKind::Go,
                command: format!("go run {target}"),
                dir: relative(root, dir),
                env: BTreeMap::new(),
                ports: Vec::new(),
            });
        }
    }
}

/// `name = "..."` from the `[package]` table of a Cargo manifest.
fn cargo_package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package
            && let Some(value) = line.strip_prefix("name")
            && let Some(value) = value.trim_start().strip_prefix('=')
        {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

fn detect_rust(root: &Path, out: &mut Vec<ServiceSpec>) {
    for manifest in find_files(root, &["Cargo.toml"], 3) {
        let dir = manifest.parent().unwrap_or(root);
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Some(name) = cargo_package_name(&text) else {
            continue;
        };
        let has_binary = dir.join("src/main.rs").exists() || text.contains("[[bin]]");
        if !has_binary {
            continue;
        }
        out.push(ServiceSpec {
            name: name.clone(),
            kind: ServiceKind::Rust,
            command: format!("cargo run -p {name}"),
            dir: relative(root, dir),
            env: BTreeMap::new(),
            ports: Vec::new(),
        });
    }
}

fn detect_python(root: &Path, out: &mut Vec<ServiceSpec>) {
    for marker in find_files(root, &["manage.py", "main.py", "app.py"], 2) {
        let dir = marker.parent().unwrap_or(root);
        let file = marker.file_name().unwrap_or_default().to_string_lossy();
        let Ok(text) = std::fs::read_to_string(&marker) else {
            continue;
        };
        let command = if file == "manage.py" {
            "python manage.py runserver".to_string()
        } else if text.contains("FastAPI(") {
            let module = file.trim_end_matches(".py");
            format!("uvicorn {module}:app --reload")
        } else if text.contains("Flask(") {
            format!("flask --app {file} run --debug")
        } else {
            continue;
        };
        out.push(ServiceSpec {
            name: dir_name(root, dir),
            kind: ServiceKind::Python,
            command,
            dir: relative(root, dir),
            env: BTreeMap::new(),
            ports: Vec::new(),
        });
    }
}

/// Services and published ports from a compose file. Handles the common
/// short syntax (`- "8080:80"`, `- 127.0.0.1:5432:5432`); anything more
/// exotic is left to `docker compose` itself.
pub fn parse_compose(text: &str) -> Vec<(String, Vec<u16>)> {
    let mut services: Vec<(String, Vec<u16>)> = Vec::new();
    let mut in_services = false;
    let mut service_indent: Option<usize> = None;
    let mut in_ports = false;
    for raw in text.lines() {
        let without_comment = raw.split(" #").next().unwrap_or(raw);
        if without_comment.trim().is_empty() || without_comment.trim_start().starts_with('#') {
            continue;
        }
        let indent = without_comment.len() - without_comment.trim_start().len();
        let line = without_comment.trim();
        if indent == 0 {
            in_services = line == "services:";
            service_indent = None;
            in_ports = false;
            continue;
        }
        if !in_services {
            continue;
        }
        let at_service_level = match service_indent {
            None => {
                service_indent = Some(indent);
                true
            }
            Some(level) => indent == level,
        };
        if at_service_level {
            if let Some(name) = line.strip_suffix(':') {
                services.push((name.trim_matches('"').to_string(), Vec::new()));
            }
            in_ports = false;
            continue;
        }
        if line == "ports:" {
            in_ports = true;
            continue;
        }
        if let Some(item) = line.strip_prefix("- ") {
            if in_ports && let Some((_, ports)) = services.last_mut() {
                let spec = item.trim().trim_matches(['"', '\'']);
                let parts: Vec<&str> = spec.split(':').collect();
                // host port is the second to last part when there is a mapping.
                let host = if parts.len() >= 2 {
                    parts[parts.len() - 2]
                } else {
                    parts[0]
                };
                if let Ok(port) = host.split('-').next().unwrap_or(host).parse() {
                    ports.push(port);
                }
            }
            continue;
        }
        if line.ends_with(':') || line.contains(": ") {
            in_ports = false;
        }
    }
    services
}

fn detect_compose(root: &Path, out: &mut Vec<ServiceSpec>) {
    let names = [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ];
    for file in find_files(root, &names, 1) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let dir = file.parent().unwrap_or(root);
        let file_name = file.file_name().unwrap_or_default().to_string_lossy();
        for (name, ports) in parse_compose(&text) {
            out.push(ServiceSpec {
                command: format!("docker compose -f {file_name} up {name}"),
                name,
                kind: ServiceKind::Compose,
                dir: relative(root, dir),
                env: BTreeMap::new(),
                ports,
            });
        }
    }
}

#[derive(Deserialize)]
struct CustomFile {
    #[serde(default)]
    services: Vec<CustomService>,
}

#[derive(Deserialize)]
struct CustomService {
    name: String,
    command: String,
    #[serde(default)]
    dir: Option<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    ports: Vec<u16>,
}

pub const CUSTOM_FILE: &str = ".solder/services.json";

/// All services of a project. Custom entries replace detected ones with the
/// same name. Names are made unique by suffixing the folder.
pub fn detect(root: &Path) -> (Vec<ServiceSpec>, Option<String>) {
    let mut found = Vec::new();
    detect_node(root, &mut found);
    detect_go(root, &mut found);
    detect_rust(root, &mut found);
    detect_python(root, &mut found);
    detect_compose(root, &mut found);

    let mut error = None;
    if let Ok(text) = std::fs::read_to_string(root.join(CUSTOM_FILE)) {
        match serde_json::from_str::<CustomFile>(&text) {
            Ok(custom) => {
                for c in custom.services {
                    found.retain(|s| s.name != c.name);
                    found.push(ServiceSpec {
                        name: c.name,
                        kind: ServiceKind::Custom,
                        command: c.command,
                        dir: c.dir.unwrap_or_default(),
                        env: c.env,
                        ports: c.ports,
                    });
                }
            }
            Err(e) => error = Some(format!("{CUSTOM_FILE}: {e}")),
        }
    }

    // Two packages called "web" in different folders become "web (apps/web)".
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for s in &found {
        *counts.entry(s.name.clone()).or_default() += 1;
    }
    for s in &mut found {
        if counts[&s.name] > 1 {
            s.name = format!("{} ({})", s.name, s.dir.display());
        }
    }
    (found, error)
}

/// Local ports a service announced in its output: `http://localhost:3000`,
/// `127.0.0.1:8080`, "listening on port 5173".
pub fn ports_in(text: &str) -> Vec<u16> {
    static URL: OnceLock<Regex> = OnceLock::new();
    static WORDS: OnceLock<Regex> = OnceLock::new();
    let url = URL.get_or_init(|| {
        Regex::new(r"(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\]):(\d{2,5})\b").unwrap()
    });
    let words = WORDS.get_or_init(|| Regex::new(r"(?i)\bport\s+(\d{2,5})\b").unwrap());
    let mut ports: Vec<u16> = url
        .captures_iter(text)
        .chain(words.captures_iter(text))
        .filter_map(|c| c[1].parse().ok())
        .filter(|p| *p >= 80)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Container {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Names")]
    pub name: String,
    #[serde(rename = "Image")]
    pub image: String,
    #[serde(rename = "State")]
    pub state: String,
    #[serde(rename = "Status")]
    pub status: String,
    #[serde(rename = "Ports", default)]
    pub ports: String,
}

impl Container {
    pub fn running(&self) -> bool {
        self.state == "running"
    }
}

/// `docker ps -a --format '{{json .}}'`: one JSON object per line.
pub fn parse_docker_ps(out: &str) -> Vec<Container> {
    out.lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect()
}

/// Lists containers. `Err` carries a short reason (no Docker, daemon off).
pub fn list_containers() -> Result<Vec<Container>, String> {
    let out = std::process::Command::new(docker_path().ok_or("Docker is not installed")?)
        .args(["ps", "-a", "--format", "{{json .}}"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(docker_error(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(parse_docker_ps(&String::from_utf8_lossy(&out.stdout)))
}

/// A short reason from docker's stderr. The CLI is the same for Docker
/// Desktop, OrbStack and Colima; the socket path in the message says which
/// runtime is down.
fn docker_error(stderr: &str) -> String {
    if !(stderr.contains("Cannot connect") || stderr.contains("daemon")) {
        return stderr.lines().next().unwrap_or("docker failed").to_string();
    }
    let runtime = if stderr.contains(".orbstack") {
        "OrbStack"
    } else if stderr.contains(".colima") {
        "Colima"
    } else {
        "Docker"
    };
    format!("{runtime} is not running")
}

pub fn container_action(id: &str, action: &str) -> Result<(), String> {
    let out = std::process::Command::new(docker_path().ok_or("Docker is not installed")?)
        .args([action, id])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("docker failed")
            .to_string())
    }
}

/// The docker binary. Apps started from the Dock get a minimal `PATH`, so
/// the usual install locations are checked too.
pub fn docker_path() -> Option<PathBuf> {
    docker_candidates(std::env::var_os("PATH"), std::env::var_os("HOME"))
        .into_iter()
        .find(|p| p.is_file())
}

fn docker_candidates(path: Option<OsString>, home: Option<OsString>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(
        [
            "/usr/local/bin",
            "/opt/homebrew/bin",
            "/Applications/Docker.app/Contents/Resources/bin",
            // OrbStack links into /usr/local/bin only when given admin rights.
            "/Applications/OrbStack.app/Contents/MacOS/xbin",
        ]
        .map(PathBuf::from),
    );
    if let Some(home) = home {
        dirs.push(Path::new(&home).join(".orbstack/bin"));
    }
    dirs.into_iter().map(|d| d.join("docker")).collect()
}

/// The shell that runs service commands: the user's login shell with `-lc`.
pub fn login_shell() -> (String, Vec<String>) {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| Path::new(s).is_file())
        .unwrap_or_else(|| "/bin/sh".into());
    (shell, vec!["-lc".into()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("solder-svc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (path, content) in files {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        dir
    }

    fn commands(root: &Path) -> Vec<(String, String, String)> {
        detect(root)
            .0
            .into_iter()
            .map(|s| (s.name, s.command, s.dir.display().to_string()))
            .collect()
    }

    #[test]
    fn detects_node_scripts_with_the_right_manager() {
        let root = project(
            "node",
            &[
                ("pnpm-lock.yaml", ""),
                ("package.json", r#"{"name":"root","scripts":{"build":"x"}}"#),
                (
                    "apps/web/package.json",
                    r#"{"name":"@acme/web","scripts":{"dev":"next dev","start":"next start"}}"#,
                ),
                (
                    "apps/api/package.json",
                    r#"{"scripts":{"start":"node server.js"}}"#,
                ),
                ("node_modules/x/package.json", r#"{"scripts":{"dev":"no"}}"#),
            ],
        );
        assert_eq!(
            commands(&root),
            vec![
                ("api".into(), "pnpm run start".into(), "apps/api".into()),
                ("web".into(), "pnpm run dev".into(), "apps/web".into()),
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_go_rust_and_python_entry_points() {
        let root = project(
            "mixed",
            &[
                ("api/go.mod", "module x"),
                ("api/cmd/server/main.go", "package main\nfunc main() {}"),
                ("api/cmd/lib/lib.go", "package lib"),
                ("worker/Cargo.toml", "[package]\nname = \"worker\"\n"),
                ("worker/src/main.rs", "fn main() {}"),
                ("shared/Cargo.toml", "[package]\nname = \"shared\"\n"),
                ("shared/src/lib.rs", ""),
                ("ml/main.py", "app = FastAPI()"),
            ],
        );
        assert_eq!(
            commands(&root),
            vec![
                ("server".into(), "go run ./cmd/server".into(), "api".into()),
                (
                    "worker".into(),
                    "cargo run -p worker".into(),
                    "worker".into()
                ),
                ("ml".into(), "uvicorn main:app --reload".into(), "ml".into()),
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_compose_services_and_ports() {
        let yaml = r#"
version: "3"
services:
  db:
    image: postgres:17
    ports:
      - "5432:5432"
    environment:
      POSTGRES_PASSWORD: x
  cache:
    image: redis
    ports:
      - 127.0.0.1:6380:6379
      - 9000
  worker:
    build: .
volumes:
  data:
"#;
        assert_eq!(
            parse_compose(yaml),
            vec![
                ("db".into(), vec![5432]),
                ("cache".into(), vec![6380, 9000]),
                ("worker".into(), vec![]),
            ]
        );
    }

    #[test]
    fn custom_services_override_and_names_stay_unique() {
        let root = project(
            "custom",
            &[
                ("a/package.json", r#"{"name":"web","scripts":{"dev":"x"}}"#),
                ("b/package.json", r#"{"name":"web","scripts":{"dev":"y"}}"#),
                ("go/go.mod", ""),
                ("go/main.go", "package main"),
                (
                    ".solder/services.json",
                    r#"{"services":[{"name":"go","command":"make run","ports":[8080]}]}"#,
                ),
            ],
        );
        let (services, error) = detect(&root);
        assert!(error.is_none());
        let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["web (a)", "web (b)", "go"]);
        assert_eq!(services[2].command, "make run");
        assert_eq!(services[2].ports, vec![8080]);
        std::fs::write(root.join(CUSTOM_FILE), "{ nope").unwrap();
        assert!(detect(&root).1.unwrap().starts_with(CUSTOM_FILE));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_ports_in_logs() {
        let log = "ready on http://localhost:3000\nAPI listening on 0.0.0.0:8787\n\
                   Vite  Local: http://127.0.0.1:5173/\nListening on port 4000\n\
                   connected to db at localhost:5432 (took 12ms)\n  timeout: 30";
        assert_eq!(ports_in(log), vec![3000, 4000, 5173, 5432, 8787]);
        assert!(ports_in("no ports here: 12").is_empty());
    }

    #[test]
    fn parses_docker_ps_json() {
        let out = r#"{"ID":"abc","Names":"db-1","Image":"postgres:17","State":"running","Status":"Up 2 minutes","Ports":"0.0.0.0:5432->5432/tcp"}
{"ID":"def","Names":"old","Image":"redis","State":"exited","Status":"Exited (0) 1 hour ago","Ports":""}
not json"#;
        let containers = parse_docker_ps(out);
        assert_eq!(containers.len(), 2);
        assert!(containers[0].running());
        assert!(!containers[1].running());
        assert_eq!(containers[1].name, "old");
    }

    #[test]
    fn finds_orbstack_without_admin_links() {
        let candidates = docker_candidates(Some("/bin".into()), Some("/Users/me".into()));
        assert_eq!(candidates[0], PathBuf::from("/bin/docker"));
        assert!(candidates.contains(&PathBuf::from("/Users/me/.orbstack/bin/docker")));
        assert!(candidates.contains(&PathBuf::from(
            "/Applications/OrbStack.app/Contents/MacOS/xbin/docker"
        )));
    }

    #[test]
    fn names_the_runtime_that_is_down() {
        let orb = "Cannot connect to the Docker daemon at unix:///Users/me/.orbstack/run/docker.sock. Is the docker daemon running?";
        assert_eq!(docker_error(orb), "OrbStack is not running");
        let desktop =
            "Cannot connect to the Docker daemon at unix:///Users/me/.docker/run/docker.sock.";
        assert_eq!(docker_error(desktop), "Docker is not running");
        assert_eq!(docker_error("permission denied\nmore"), "permission denied");
    }
}
