//! The routes a project serves, read from its code the way a developer
//! would skim it: file-system routes in Next.js, route calls in Express and
//! its relatives, decorators in FastAPI and Flask, Django's urlpatterns,
//! Go's mux and router calls, axum's `.route` and actix attributes. Paths
//! use `{param}` for parameters whatever the framework wrote. Reads the
//! disk: run it on a background thread.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub method: String,
    pub path: String,
    /// Relative to the project root.
    pub file: PathBuf,
    /// 0-based line of the definition.
    pub line: usize,
    pub framework: &'static str,
}

const MAX_DEPTH: usize = 10;
const MAX_FILE: u64 = 512 * 1024;

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
                | "coverage"
        )
}

/// Every route found under `root`, sorted by path then method.
pub fn detect(root: &Path) -> Vec<Route> {
    let mut routes = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < MAX_DEPTH && !skip_dir(&name) {
                    dirs.push((path, depth + 1));
                }
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(
                ext,
                "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go" | "rs"
            ) {
                continue;
            }
            if entry.metadata().map_or(true, |m| m.len() > MAX_FILE) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            routes.extend(in_file(&rel, &text));
        }
    }
    routes.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.method.cmp(&b.method))
            .then(a.file.cmp(&b.file))
    });
    routes.dedup_by(|a, b| a.method == b.method && a.path == b.path);
    routes
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("route pattern"))
}

/// `:id`, `<int:id>`, `[id]`, `[...slug]` all become `{id}` / `{slug}`.
fn normalize(path: &str) -> String {
    static PARAMS: OnceLock<Regex> = OnceLock::new();
    let params = re(&PARAMS, r":(\w+)|<(?:\w+:)?(\w+)>|\[\.{0,3}(\w+)\]");
    let path = params.replace_all(path, |c: &regex::Captures| {
        let name = c
            .get(1)
            .or(c.get(2))
            .or(c.get(3))
            .map_or("", |m| m.as_str());
        format!("{{{name}}}")
    });
    let path = if path.starts_with('/') {
        path.into_owned()
    } else {
        format!("/{path}")
    };
    if path.len() > 1 {
        path.trim_end_matches('/').to_string()
    } else {
        path
    }
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count()
}

/// Routes defined in one file.
pub fn in_file(rel: &Path, text: &str) -> Vec<Route> {
    let mut out = Vec::new();
    let mut push = |method: &str, path: &str, offset: usize, framework: &'static str| {
        out.push(Route {
            method: method.to_ascii_uppercase(),
            path: normalize(path),
            file: rel.to_path_buf(),
            line: line_of(text, offset),
            framework,
        });
    };
    let components: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let file_name = components.last().cloned().unwrap_or_default();
    // `[...slug].ts` keeps its dots: only the extension goes.
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name.as_str(), |(s, _)| s);
    let ext = rel.extension().and_then(|e| e.to_str()).unwrap_or("");

    // Next.js App Router: app/**/route.ts exports one function per method.
    if stem == "route"
        && let Some(app) = components.iter().position(|c| c == "app")
    {
        let segments: Vec<&str> = components[app + 1..components.len() - 1]
            .iter()
            .map(String::as_str)
            // (groups) and @slots do not appear in the URL.
            .filter(|s| !(s.starts_with('@') || (s.starts_with('(') && s.ends_with(')'))))
            .collect();
        let path = format!("/{}", segments.join("/"));
        static EXPORTS: OnceLock<Regex> = OnceLock::new();
        let exports = re(
            &EXPORTS,
            r"export\s+(?:async\s+)?(?:function\s+|const\s+)(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b",
        );
        for c in exports.captures_iter(text) {
            push(&c[1], &path, c.get(0).map_or(0, |m| m.start()), "Next.js");
        }
        return out;
    }
    // Next.js Pages Router API routes: one handler for every method.
    if let Some(pages) = components
        .windows(2)
        .position(|w| w[0] == "pages" && w[1] == "api")
        && matches!(ext, "ts" | "js" | "tsx" | "jsx")
    {
        let mut segments: Vec<String> = components[pages + 1..].to_vec();
        if let Some(last) = segments.last_mut() {
            *last = stem.to_string();
        }
        if segments.last().is_some_and(|s| s == "index") {
            segments.pop();
        }
        push("GET", &format!("/{}", segments.join("/")), 0, "Next.js");
        return out;
    }

    match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            // Express, Koa routers, Fastify, Hono, Elysia.
            static CALLS: OnceLock<Regex> = OnceLock::new();
            let calls = re(
                &CALLS,
                r#"\b(?:app|router|server|api|route|routes|fastify|hono|r)\.(get|post|put|patch|delete)\(\s*['"`](/[^'"`]*)['"`]"#,
            );
            for c in calls.captures_iter(text) {
                push(&c[1], &c[2], c.get(0).map_or(0, |m| m.start()), "Express");
            }
        }
        "py" => {
            static FASTAPI: OnceLock<Regex> = OnceLock::new();
            let fastapi = re(
                &FASTAPI,
                r#"@\w+\.(get|post|put|patch|delete)\(\s*['"]([^'"]*)['"]"#,
            );
            for c in fastapi.captures_iter(text) {
                push(&c[1], &c[2], c.get(0).map_or(0, |m| m.start()), "FastAPI");
            }
            static FLASK: OnceLock<Regex> = OnceLock::new();
            let flask = re(
                &FLASK,
                r#"@\w+\.route\(\s*['"]([^'"]*)['"](?:[^)\n]*methods\s*=\s*\[([^\]]*)\])?"#,
            );
            for c in flask.captures_iter(text) {
                let at = c.get(0).map_or(0, |m| m.start());
                let methods: Vec<String> = c.get(2).map_or_else(
                    || vec!["GET".into()],
                    |m| {
                        m.as_str()
                            .split(',')
                            .map(|s| s.trim().trim_matches(['"', '\'']).to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    },
                );
                for method in methods {
                    push(&method, &c[1], at, "Flask");
                }
            }
            if file_name == "urls.py" {
                static DJANGO: OnceLock<Regex> = OnceLock::new();
                let django = re(&DJANGO, r#"\b(?:re_)?path\(\s*r?['"]([^'"]*)['"]"#);
                for c in django.captures_iter(text) {
                    push("GET", &c[1], c.get(0).map_or(0, |m| m.start()), "Django");
                }
            }
        }
        "go" => {
            // net/http (with Go 1.22's "METHOD /path" patterns).
            static MUX: OnceLock<Regex> = OnceLock::new();
            let mux = re(&MUX, r#"\bHandle(?:Func)?\(\s*"(?:([A-Z]+)\s+)?(/[^"]*)""#);
            for c in mux.captures_iter(text) {
                let method = c.get(1).map_or("GET", |m| m.as_str());
                push(method, &c[2], c.get(0).map_or(0, |m| m.start()), "Go");
            }
            // gin, echo, chi, fiber.
            static ROUTERS: OnceLock<Regex> = OnceLock::new();
            let routers = re(
                &ROUTERS,
                r#"\.(GET|POST|PUT|PATCH|DELETE|Get|Post|Put|Patch|Delete)\(\s*"(/[^"]*)""#,
            );
            for c in routers.captures_iter(text) {
                push(&c[1], &c[2], c.get(0).map_or(0, |m| m.start()), "Go");
            }
        }
        "rs" => {
            static AXUM: OnceLock<Regex> = OnceLock::new();
            let axum = re(&AXUM, r#"\.route\(\s*"([^"]+)"\s*,([^\n]*)"#);
            static METHOD: OnceLock<Regex> = OnceLock::new();
            let method = re(&METHOD, r"\b(get|post|put|patch|delete)\(");
            for c in axum.captures_iter(text) {
                let at = c.get(0).map_or(0, |m| m.start());
                for m in method.captures_iter(&c[2]) {
                    push(&m[1], &c[1], at, "axum");
                }
            }
            static ACTIX: OnceLock<Regex> = OnceLock::new();
            let actix = re(&ACTIX, r#"#\[(get|post|put|patch|delete)\(\s*"([^"]+)""#);
            for c in actix.captures_iter(text) {
                push(&c[1], &c[2], c.get(0).map_or(0, |m| m.start()), "actix");
            }
        }
        _ => {}
    }
    out
}

/// A port the project's server likely listens on: `PORT` in `.env`, else
/// the framework's usual one.
pub fn default_port(framework: Option<&str>, env_port: Option<u16>) -> u16 {
    if let Some(port) = env_port {
        return port;
    }
    match framework {
        Some("FastAPI" | "Django") => 8000,
        Some("Flask") => 5000,
        Some("Go" | "actix") => 8080,
        _ => 3000,
    }
}

/// A request for `route`, ready for a `.http` file: parameters get a
/// placeholder value, bodies an empty JSON object.
pub fn to_http(route: &Route) -> String {
    static PARAM: OnceLock<Regex> = OnceLock::new();
    let param = re(&PARAM, r"\{(\w+)\}");
    let url = param.replace_all(&route.path, |c: &regex::Captures| {
        if c[1].contains("id") {
            "1".to_string()
        } else {
            c[1].to_string()
        }
    });
    let mut out = format!(
        "### {} {} ({}:{})\n{} {{{{baseUrl}}}}{url}\n",
        route.method,
        route.path,
        route.file.display(),
        route.line + 1,
        route.method
    );
    if matches!(route.method.as_str(), "POST" | "PUT" | "PATCH") {
        out.push_str("Content-Type: application/json\n\n{}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes(file: &str, text: &str) -> Vec<(String, String)> {
        in_file(Path::new(file), text)
            .into_iter()
            .map(|r| (r.method, r.path))
            .collect()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(m, p)| (m.to_string(), p.to_string()))
            .collect()
    }

    #[test]
    fn next_js_routes() {
        let handler = "export async function GET(req) {}\nexport const POST = async () => {}\n";
        assert_eq!(
            routes("src/app/(shop)/api/users/[id]/route.ts", handler),
            pairs(&[("GET", "/api/users/{id}"), ("POST", "/api/users/{id}")])
        );
        assert_eq!(
            routes("pages/api/users/index.ts", ""),
            pairs(&[("GET", "/api/users")])
        );
        assert_eq!(
            routes("pages/api/posts/[...slug].ts", ""),
            pairs(&[("GET", "/api/posts/{slug}")])
        );
    }

    #[test]
    fn express_fastapi_flask_django() {
        let express = "router.get('/users/:id', h)\napp.post(\"/users\", h)\nfoo.get('/no')\n";
        assert_eq!(
            routes("server/routes.ts", express),
            pairs(&[("GET", "/users/{id}"), ("POST", "/users")])
        );
        let fastapi = "@app.get(\"/items/{item_id}\")\nasync def read(): ...\n@router.delete('/items/{item_id}')\n";
        assert_eq!(
            routes("main.py", fastapi),
            pairs(&[("GET", "/items/{item_id}"), ("DELETE", "/items/{item_id}")])
        );
        let flask = "@app.route('/login', methods=['GET', 'POST'])\n@bp.route(\"/u/<int:id>\")\n";
        assert_eq!(
            routes("app.py", flask),
            pairs(&[("GET", "/login"), ("POST", "/login"), ("GET", "/u/{id}")])
        );
        let django = "urlpatterns = [\n  path('articles/<int:year>/', views.year),\n  path('', views.home),\n]";
        assert_eq!(
            routes("blog/urls.py", django),
            pairs(&[("GET", "/articles/{year}"), ("GET", "/")])
        );
    }

    #[test]
    fn go_and_rust() {
        let go = "mux.HandleFunc(\"POST /users/{id}\", h)\nhttp.HandleFunc(\"/health\", h)\nr.GET(\"/items/:id\", h)\ne.Delete(\"/x\", h)\n";
        assert_eq!(
            routes("main.go", go),
            pairs(&[
                ("POST", "/users/{id}"),
                ("GET", "/health"),
                ("GET", "/items/{id}"),
                ("DELETE", "/x")
            ])
        );
        let axum = "Router::new()\n    .route(\"/users/{id}\", get(show).put(update))\n    .route(\"/health\", get(health));\n#[post(\"/hooks\")]\nasync fn hook() {}\n";
        assert_eq!(
            routes("src/main.rs", axum),
            pairs(&[
                ("GET", "/users/{id}"),
                ("PUT", "/users/{id}"),
                ("GET", "/health"),
                ("POST", "/hooks")
            ])
        );
    }

    #[test]
    fn whole_project_and_requests() {
        let root = std::env::temp_dir().join(format!("solder-routes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("app/api/users")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(
            root.join("app/api/users/route.ts"),
            "export async function POST() {}",
        )
        .unwrap();
        std::fs::write(
            root.join("node_modules/x/index.js"),
            "app.get('/hidden', h)",
        )
        .unwrap();
        let found = detect(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file, Path::new("app/api/users/route.ts"));
        assert_eq!(default_port(found.first().map(|r| r.framework), None), 3000);
        assert_eq!(default_port(Some("FastAPI"), Some(4000)), 4000);
        assert_eq!(default_port(Some("FastAPI"), None), 8000);
        let route = Route {
            method: "PUT".into(),
            path: "/users/{id}/posts/{slug}".into(),
            file: "src/main.rs".into(),
            line: 9,
            framework: "axum",
        };
        assert_eq!(
            to_http(&route),
            "### PUT /users/{id}/posts/{slug} (src/main.rs:10)\nPUT {{baseUrl}}/users/1/posts/slug\nContent-Type: application/json\n\n{}\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
