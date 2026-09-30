//! Finding a project's databases: connection URLs in `.env` files, database
//! images in compose files, SQLite files, and `.solder/connections.json`.
//! Everything here reads the disk: run it on a background thread.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use yaml_rust2::{Yaml, YamlLoader};

use crate::{ConnectionSpec, Engine};

pub const CUSTOM_FILE: &str = ".solder/connections.json";
const MAX_DEPTH: usize = 3;
const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// Every connection found under `root`, custom ones first. `user_file` holds
/// connections added in the editor (outside the project, so their passwords
/// are never committed). Names are unique.
pub fn detect(root: &Path, user_file: Option<&Path>) -> Vec<ConnectionSpec> {
    let root_env = read_env(&root.join(".env"));
    let mut found = Vec::new();
    if let Some(text) = user_file.and_then(|f| std::fs::read_to_string(f).ok()) {
        found.extend(parse_custom(&text, root, "added in Solder", &root_env));
    }
    if let Ok(text) = std::fs::read_to_string(root.join(CUSTOM_FILE)) {
        found.extend(parse_custom(&text, root, CUSTOM_FILE, &root_env));
    }
    let files = walk(root);
    for path in &files {
        let name = file_name(path);
        let rel = relative(root, path);
        if is_env_file(&name) {
            let dir = path.parent().unwrap_or(root);
            let vars = read_env(path);
            let production = name.contains("prod");
            let mut keys: Vec<_> = vars.keys().collect();
            keys.sort();
            for key in keys {
                let value = &vars[key];
                let Some(engine) = Engine::from_url(value) else {
                    continue;
                };
                let Some(url) = resolve_url(engine, value, dir, root) else {
                    continue;
                };
                found.push(ConnectionSpec {
                    name: key.clone(),
                    engine,
                    url,
                    source: rel.clone(),
                    read_only: production || key.to_ascii_lowercase().contains("prod"),
                });
            }
        } else if COMPOSE_FILES.contains(&name.as_str()) {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let mut env = read_env(&path.with_file_name(".env"));
            for (k, v) in std::env::vars() {
                env.entry(k).or_insert(v);
            }
            found.extend(parse_compose(&text, &rel, &env));
        } else if is_sqlite_name(&name) && has_sqlite_header(path) {
            found.push(ConnectionSpec {
                name: name.clone(),
                engine: Engine::Sqlite,
                url: path.display().to_string(),
                source: rel,
                read_only: false,
            });
        }
    }
    dedupe(found)
}

/// Drops repeated URLs (the first source wins) and makes names unique.
fn dedupe(found: Vec<ConnectionSpec>) -> Vec<ConnectionSpec> {
    let mut out: Vec<ConnectionSpec> = Vec::new();
    for spec in found {
        if out.iter().any(|s| s.url == spec.url) {
            continue;
        }
        out.push(spec);
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for spec in &out {
        *counts.entry(spec.name.clone()).or_default() += 1;
    }
    for spec in &mut out {
        if counts[&spec.name] > 1 {
            spec.name = format!("{} ({})", spec.name, spec.source);
        }
    }
    out
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
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
                if depth < MAX_DEPTH && !skip_dir(&name) {
                    dirs.push((entry.path(), depth + 1));
                }
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

fn skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "node_modules"
                | "target"
                | "dist"
                | "build"
                | "out"
                | "vendor"
                | "venv"
                | "__pycache__"
        )
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn is_env_file(name: &str) -> bool {
    (name == ".env" || name.starts_with(".env."))
        && !["example", "sample", "template", "dist", "vault"]
            .iter()
            .any(|s| name.ends_with(s))
}

fn is_sqlite_name(name: &str) -> bool {
    [".sqlite", ".sqlite3", ".db", ".db3"]
        .iter()
        .any(|ext| name.ends_with(ext))
}

fn has_sqlite_header(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .is_ok()
        && &header == b"SQLite format 3\0"
}

fn read_env(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .map(|text| parse_env(&text))
        .unwrap_or_default()
}

/// `KEY=value` lines as dotenv reads them: `export`, quotes, comments and
/// `${OTHER}` references to earlier keys.
pub fn parse_env(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let value = value.trim();
        let value = if let Some(inner) = value.strip_prefix('"') {
            let inner = inner.rsplit_once('"').map_or(inner, |(v, _)| v);
            interpolate(&inner.replace("\\n", "\n"), &vars)
        } else if let Some(inner) = value.strip_prefix('\'') {
            inner
                .rsplit_once('\'')
                .map_or(inner, |(v, _)| v)
                .to_string()
        } else {
            let value = value.split(" #").next().unwrap_or(value).trim();
            interpolate(value, &vars)
        };
        vars.insert(key.to_string(), value);
    }
    vars
}

/// Expands `${NAME}`, `${NAME:-default}`, `${NAME-default}` and `$NAME`.
fn interpolate(value: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(ix) = rest.find('$') {
        out.push_str(&rest[..ix]);
        let after = &rest[ix + 1..];
        if let Some(inner) = after.strip_prefix('{') {
            let Some(end) = inner.find('}') else {
                out.push_str(&rest[ix..]);
                return out;
            };
            let expr = &inner[..end];
            let (name, default) = match expr.split_once(":-").or_else(|| expr.split_once('-')) {
                Some((name, default)) => (name, Some(default)),
                None => (expr, None),
            };
            match vars.get(name).filter(|v| !v.is_empty()) {
                Some(v) => out.push_str(v),
                None => out.push_str(default.unwrap_or("")),
            }
            rest = &inner[end + 1..];
        } else {
            let len = after
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            if len == 0 {
                out.push('$');
            } else if let Some(v) = vars.get(&after[..len]) {
                out.push_str(v);
            }
            rest = &after[len..];
        }
    }
    out.push_str(rest);
    out
}

/// SQLite URLs become absolute paths. Relative ones are relative to the file
/// that names them; Prisma's `file:./dev.db` is relative to `prisma/`.
fn resolve_url(engine: Engine, value: &str, dir: &Path, root: &Path) -> Option<String> {
    if engine != Engine::Sqlite {
        return Some(value.to_string());
    }
    let path = value
        .strip_prefix("sqlite://")
        .or_else(|| value.strip_prefix("sqlite:"))
        .or_else(|| value.strip_prefix("sqlite3:"))
        .or_else(|| value.strip_prefix("file:"))
        .unwrap_or(value);
    let path = path.split('?').next().unwrap_or(path);
    if path.is_empty() || path == ":memory:" {
        return None;
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Some(path.display().to_string());
    }
    let candidates = [
        dir.join(path),
        dir.join("prisma").join(path),
        root.join(path),
    ];
    let found = candidates
        .iter()
        .find(|p| p.exists())
        .unwrap_or(&candidates[0]);
    Some(normalize(found).display().to_string())
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `.solder/connections.json`:
/// `{"connections": [{"name": "prod", "url": "postgres://...", "readOnly": true}]}`.
/// `${VAR}` in a URL comes from the project's `.env`. Entries with a
/// `project` other than `root` are skipped.
pub fn parse_custom(
    text: &str,
    root: &Path,
    source: &str,
    env: &HashMap<String, String>,
) -> Vec<ConnectionSpec> {
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        connections: Vec<Entry>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Entry {
        name: String,
        url: String,
        #[serde(default)]
        read_only: bool,
        /// Set in the user's own file, which holds every project's entries.
        project: Option<std::path::PathBuf>,
    }
    let Ok(file) = serde_json::from_str::<File>(text) else {
        return Vec::new();
    };
    file.connections
        .into_iter()
        .filter(|entry| entry.project.as_ref().is_none_or(|p| p == root))
        .filter_map(|entry| {
            let url = interpolate(&entry.url, env);
            let engine = Engine::from_url(&url).or_else(|| {
                (url.starts_with('/') || is_sqlite_name(&url)).then_some(Engine::Sqlite)
            })?;
            Some(ConnectionSpec {
                name: entry.name,
                engine,
                url: resolve_url(engine, &url, root, root)?,
                source: source.to_string(),
                read_only: entry.read_only,
            })
        })
        .collect()
}

/// Database services in a compose file, reachable on their published ports.
fn parse_compose(text: &str, source: &str, env: &HashMap<String, String>) -> Vec<ConnectionSpec> {
    let Ok(docs) = YamlLoader::load_from_str(text) else {
        return Vec::new();
    };
    let Some(services) = docs.first().and_then(|d| d["services"].as_hash()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, service) in services {
        let (Some(name), Some(image)) = (name.as_str(), service["image"].as_str()) else {
            continue;
        };
        let image = interpolate(image, env);
        let Some(engine) = image_engine(&image) else {
            continue;
        };
        let vars = service_env(&service["environment"], env);
        let get = |keys: &[&str]| {
            keys.iter()
                .find_map(|k| vars.get(*k).filter(|v| !v.is_empty()).cloned())
        };
        let default_port = match engine {
            Engine::Postgres => 5432,
            Engine::MySql => 3306,
            Engine::Redis => 6379,
            Engine::Mongo => 27017,
            Engine::Sqlite => continue,
        };
        let Some(port) = published_port(&service["ports"], default_port, env) else {
            continue;
        };
        let url = match engine {
            Engine::Postgres => {
                let user = get(&["POSTGRES_USER"]).unwrap_or_else(|| "postgres".into());
                let password = get(&["POSTGRES_PASSWORD"]);
                let db = get(&["POSTGRES_DB"]).unwrap_or_else(|| user.clone());
                format!(
                    "postgres://{}@localhost:{port}/{}",
                    userinfo(&user, password.as_deref()),
                    encode(&db)
                )
            }
            Engine::MySql => {
                let (user, password) = match get(&["MYSQL_USER", "MARIADB_USER"]) {
                    Some(user) => (user, get(&["MYSQL_PASSWORD", "MARIADB_PASSWORD"])),
                    None => (
                        "root".into(),
                        get(&["MYSQL_ROOT_PASSWORD", "MARIADB_ROOT_PASSWORD"]),
                    ),
                };
                let db = get(&["MYSQL_DATABASE", "MARIADB_DATABASE"]).unwrap_or_default();
                format!(
                    "mysql://{}@localhost:{port}/{}",
                    userinfo(&user, password.as_deref()),
                    encode(&db)
                )
            }
            Engine::Redis => match get(&["REDIS_PASSWORD"]) {
                Some(password) => format!("redis://:{}@localhost:{port}", encode(&password)),
                None => format!("redis://localhost:{port}"),
            },
            Engine::Mongo => {
                let db = get(&["MONGO_INITDB_DATABASE"]).unwrap_or_default();
                match get(&["MONGO_INITDB_ROOT_USERNAME"]) {
                    Some(user) => format!(
                        "mongodb://{}@localhost:{port}/{}?authSource=admin",
                        userinfo(&user, get(&["MONGO_INITDB_ROOT_PASSWORD"]).as_deref()),
                        encode(&db)
                    ),
                    None => format!("mongodb://localhost:{port}/{}", encode(&db)),
                }
            }
            Engine::Sqlite => continue,
        };
        out.push(ConnectionSpec {
            name: name.to_string(),
            engine,
            url,
            source: source.to_string(),
            read_only: false,
        });
    }
    out
}

fn image_engine(image: &str) -> Option<Engine> {
    let without_tag = image.split(['@']).next().unwrap_or(image);
    let without_tag = match without_tag.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => name,
        _ => without_tag,
    };
    let base = without_tag.rsplit('/').next().unwrap_or(without_tag);
    Some(match base {
        "postgres" | "postgis" | "timescaledb" | "timescaledb-ha" | "pgvector" | "postgresql" => {
            Engine::Postgres
        }
        "mysql" | "mariadb" | "percona" | "percona-server" | "mysql-server" => Engine::MySql,
        "redis" | "valkey" | "keydb" | "redis-stack" | "redis-stack-server" => Engine::Redis,
        "mongo" | "mongodb" | "mongodb-community-server" => Engine::Mongo,
        _ => return None,
    })
}

fn service_env(node: &Yaml, env: &HashMap<String, String>) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    match node {
        Yaml::Hash(map) => {
            for (k, v) in map {
                let value = match v {
                    Yaml::String(s) => s.clone(),
                    Yaml::Integer(i) => i.to_string(),
                    Yaml::Real(r) => r.clone(),
                    Yaml::Boolean(b) => b.to_string(),
                    _ => continue,
                };
                if let Some(k) = k.as_str() {
                    vars.insert(k.to_string(), interpolate(&value, env));
                }
            }
        }
        Yaml::Array(items) => {
            for item in items.iter().filter_map(Yaml::as_str) {
                if let Some((k, v)) = item.split_once('=') {
                    vars.insert(k.to_string(), interpolate(v, env));
                }
            }
        }
        _ => {}
    }
    vars
}

/// The host port that maps to `target`: `"5433:5432"`, `"127.0.0.1:5433:5432"`
/// or `{target: 5432, published: 5433}`. Unpublished ports are unreachable
/// from the editor.
fn published_port(node: &Yaml, target: u16, env: &HashMap<String, String>) -> Option<u16> {
    for item in node.as_vec()? {
        match item {
            Yaml::String(_) | Yaml::Integer(_) => {
                let spec = match item {
                    Yaml::String(s) => interpolate(s, env),
                    Yaml::Integer(i) => i.to_string(),
                    _ => unreachable!(),
                };
                let spec = spec.split('/').next().unwrap_or(&spec);
                let parts: Vec<&str> = spec.split(':').collect();
                if parts.len() < 2 || parts[parts.len() - 1].parse() != Ok(target) {
                    continue;
                }
                if let Ok(port) = parts[parts.len() - 2].parse() {
                    return Some(port);
                }
            }
            Yaml::Hash(_) => {
                let port = |key: &str| match &item[key] {
                    Yaml::Integer(i) => u16::try_from(*i).ok(),
                    Yaml::String(s) => interpolate(s, env).parse().ok(),
                    _ => None,
                };
                if port("target") == Some(target)
                    && let Some(published) = port("published")
                {
                    return Some(published);
                }
            }
            _ => {}
        }
    }
    None
}

fn userinfo(user: &str, password: Option<&str>) -> String {
    match password {
        Some(p) => format!("{}:{}", encode(user), encode(p)),
        None => encode(user),
    }
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("solder-db-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn env_files_quotes_comments_and_references() {
        let vars = parse_env(
            "# comment\nexport HOST=db.local\nPASS='p#ss'\n\
             DATABASE_URL=\"postgres://app:${PASS}@${HOST}:5432/app\"\n\
             REDIS_URL=redis://localhost:6379 # cache\nEMPTY=\n",
        );
        assert_eq!(
            vars["DATABASE_URL"],
            "postgres://app:p#ss@db.local:5432/app"
        );
        assert_eq!(vars["REDIS_URL"], "redis://localhost:6379");
        assert_eq!(vars["EMPTY"], "");
        assert_eq!(
            interpolate("${MISSING:-fallback}/$HOST", &vars),
            "fallback/db.local"
        );
    }

    #[test]
    fn compose_databases_on_published_ports() {
        let env: HashMap<String, String> = [("PG_PORT".to_string(), "5433".to_string())].into();
        let compose = r#"
services:
  db:
    image: postgres:17-alpine
    environment:
      POSTGRES_USER: app
      POSTGRES_PASSWORD: "s3cret!"
    ports:
      - "${PG_PORT}:5432"
  mysql:
    image: docker.io/library/mysql:8.4
    environment:
      - MYSQL_ROOT_PASSWORD=root
      - MYSQL_DATABASE=shop
    ports: ["3307:3306"]
  cache:
    image: redis:7
    ports:
      - target: 6379
        published: 6380
  mongo:
    image: mongo
    environment:
      MONGO_INITDB_ROOT_USERNAME: root
      MONGO_INITDB_ROOT_PASSWORD: pw
    ports: ["27017:27017"]
  hidden:
    image: postgres
  web:
    image: node:22
    ports: ["3000:3000"]
"#;
        let specs = parse_compose(compose, "compose.yaml", &env);
        let urls: Vec<(&str, &str)> = specs
            .iter()
            .map(|s| (s.name.as_str(), s.url.as_str()))
            .collect();
        assert_eq!(
            urls,
            vec![
                ("db", "postgres://app:s3cret%21@localhost:5433/app"),
                ("mysql", "mysql://root:root@localhost:3307/shop"),
                ("cache", "redis://localhost:6380"),
                (
                    "mongo",
                    "mongodb://root:pw@localhost:27017/?authSource=admin"
                ),
            ]
        );
        assert_eq!(specs[0].engine, Engine::Postgres);
        assert_eq!(specs[3].engine, Engine::Mongo);
    }

    #[test]
    fn detects_env_compose_sqlite_and_custom() {
        let root = fixture("detect");
        std::fs::write(
            root.join(".env"),
            "DATABASE_URL=postgres://localhost:5432/app\nSECRET=x\nDEV_DB=file:./dev.db\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".env.production"),
            "DATABASE_URL=postgres://prod.example.com/app\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".env.example"),
            "DATABASE_URL=postgres://example/app\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("prisma")).unwrap();
        std::fs::write(root.join("prisma/dev.db"), b"SQLite format 3\0rest").unwrap();
        std::fs::write(root.join("notes.db"), b"not sqlite").unwrap();
        std::fs::write(
            root.join("docker-compose.yml"),
            "services:\n  db:\n    image: postgres\n    ports: [\"5432:5432\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".solder")).unwrap();
        std::fs::write(
            root.join(CUSTOM_FILE),
            r#"{"connections": [{"name": "analytics", "url": "mysql://ro@${DB_HOST:-10.0.0.2}/stats", "readOnly": true}]}"#,
        )
        .unwrap();

        let specs = detect(&root, None);
        let summary: Vec<(&str, Engine, &str, bool)> = specs
            .iter()
            .map(|s| (s.name.as_str(), s.engine, s.source.as_str(), s.read_only))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("analytics", Engine::MySql, CUSTOM_FILE, true),
                ("DATABASE_URL (.env)", Engine::Postgres, ".env", false),
                ("DEV_DB", Engine::Sqlite, ".env", false),
                (
                    "DATABASE_URL (.env.production)",
                    Engine::Postgres,
                    ".env.production",
                    true
                ),
                ("db", Engine::Postgres, "docker-compose.yml", false),
            ]
        );
        assert_eq!(specs[0].url, "mysql://ro@10.0.0.2/stats");
        // Prisma's relative path, resolved; the same file is not listed twice.
        assert_eq!(
            specs[2].url,
            root.join("prisma/dev.db").display().to_string()
        );
    }

    #[test]
    fn masks_passwords() {
        let spec = ConnectionSpec {
            name: "db".into(),
            engine: Engine::Postgres,
            url: "postgres://app:secret@localhost/app".into(),
            source: ".env".into(),
            read_only: false,
        };
        assert!(!spec.display_url().contains("secret"));
        assert!(spec.display_url().contains("app:"));
    }
}
