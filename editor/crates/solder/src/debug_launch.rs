//! What can be debugged in a project: each `package.json` script, the open
//! file, and for a Next.js app its server and the browser together.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaunchConfig {
    pub name: String,
    /// The launch request's arguments for js-debug.
    pub request: Value,
    /// A page to open in a browser session once the server answers there.
    pub browser: Option<String>,
    /// The debug adapter of an extension that runs it, in place of
    /// js-debug: the extension then says what the request is.
    pub adapter: Option<ExtensionAdapter>,
}

/// A debug adapter an installed extension brings, and what to debug with it.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionAdapter {
    /// The extension's id.
    pub extension: String,
    pub launch: extension::host::DebugLaunch,
    /// The launch as it came whole from the extension's own code, which
    /// then is not put together from its manifest.
    pub configuration: Option<Value>,
}

/// The open file with each debug adapter that installed extensions bring
/// for its language: `rdbg app.rb`.
pub fn from_extensions(root: &Path, file: &Path, cx: &gpui::App) -> Vec<LaunchConfig> {
    // A file may have no language here and still have a debugger: one a
    // VS Code extension declares goes by the file's name.
    let language = syntax::language_for_path(file);
    let Some(store) = crate::extension_store::ExtensionStore::try_global(cx) else {
        return Vec::new();
    };
    let shown = file
        .strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string();
    store
        .read(cx)
        .debuggers_for_file(file, language.as_ref().map(|language| language.name))
        .into_iter()
        .map(|(extension, adapter)| {
            let name = format!("{adapter} {shown}");
            LaunchConfig {
                name: name.clone(),
                adapter: Some(ExtensionAdapter {
                    extension,
                    launch: extension::host::DebugLaunch {
                        label: name,
                        adapter,
                        program: file.display().to_string(),
                        cwd: Some(root.display().to_string()),
                        args: Vec::new(),
                        env: Vec::new(),
                    },
                    configuration: None,
                }),
                ..Default::default()
            }
        })
        .collect()
}

/// Node and the debugged program skip Node's own code when stepping.
fn node_config(name: &str, cwd: &Path) -> Value {
    json!({
        "type": "pwa-node",
        "request": "launch",
        "name": name,
        "cwd": cwd,
        "console": "internalConsole",
        "outputCapture": "std",
        "skipFiles": ["<node_internals>/**"],
        // Source maps of the project only: a package manager's own files
        // otherwise fill the console with warnings about missing maps.
        "resolveSourceMapLocations": [format!("{}/**", cwd.display()), "!**/node_modules/**"],
        "env": {},
    })
}

/// The port a dev script serves on: `-p 4000`, `--port=4000`, else the
/// framework's default.
fn script_port(script: &str, default: u16) -> u16 {
    let words: Vec<&str> = script.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        if let Some(v) = w.strip_prefix("--port=")
            && let Ok(p) = v.parse()
        {
            return p;
        }
        if matches!(*w, "-p" | "--port")
            && let Some(p) = words.get(i + 1).and_then(|v| v.parse().ok())
        {
            return p;
        }
    }
    default
}

pub fn detect(root: &Path, file: Option<&Path>) -> Vec<LaunchConfig> {
    #[derive(serde::Deserialize)]
    struct Package {
        #[serde(default)]
        scripts: BTreeMap<String, String>,
        #[serde(default)]
        dependencies: BTreeMap<String, Value>,
        #[serde(default, rename = "devDependencies")]
        dev_dependencies: BTreeMap<String, Value>,
    }
    let mut out = Vec::new();
    if let Some(file) = file.filter(|f| {
        f.extension()
            .is_some_and(|e| matches!(e.to_str(), Some("js" | "mjs" | "cjs" | "ts" | "mts")))
    }) {
        let shown = file
            .strip_prefix(root)
            .unwrap_or(file)
            .display()
            .to_string();
        let mut request = node_config(&shown, file.parent().unwrap_or(root));
        request["program"] = file.display().to_string().into();
        out.push(LaunchConfig {
            name: format!("node {shown}"),
            request,
            ..Default::default()
        });
    }
    let mut manifests = crate::services::find_files(root, &["package.json"], 3);
    manifests.sort();
    for manifest in manifests {
        let Some(package) = std::fs::read_to_string(&manifest)
            .ok()
            .and_then(|t| serde_json::from_str::<Package>(&t).ok())
        else {
            continue;
        };
        let dir: PathBuf = manifest.parent().unwrap_or(root).to_path_buf();
        let manager = crate::services::node_manager(&dir, root);
        let next = package.dependencies.contains_key("next")
            || package.dev_dependencies.contains_key("next");
        let prefix = match dir.strip_prefix(root) {
            Ok(rel) if !rel.as_os_str().is_empty() => format!("{}: ", rel.display()),
            _ => String::new(),
        };
        // The usual entry points first, then the rest in name order.
        let mut names: Vec<&String> = package.scripts.keys().collect();
        names.sort_by_key(|n| {
            (
                ["dev", "start", "test"]
                    .iter()
                    .position(|p| p == n)
                    .unwrap_or(9),
                n.to_string(),
            )
        });
        for script in names {
            let body = &package.scripts[script];
            let label = match manager {
                "yarn" => format!("yarn {script}"),
                m => format!("{m} run {script}"),
            };
            let mut request = node_config(&format!("{prefix}{label}"), &dir);
            request["runtimeExecutable"] = manager.into();
            request["runtimeArgs"] = if manager == "yarn" {
                json!([script])
            } else {
                json!(["run", script])
            };
            let browser = (next && body.contains("next dev"))
                .then(|| format!("http://localhost:{}", script_port(body, 3000)));
            if let Some(url) = &browser {
                out.push(LaunchConfig {
                    name: format!("{prefix}{label} + browser"),
                    request: request.clone(),
                    browser: Some(url.clone()),
                    adapter: None,
                });
            }
            out.push(LaunchConfig {
                name: format!("{prefix}{label}"),
                request,
                ..Default::default()
            });
        }
    }
    out
}

/// The browser half of a server-and-browser session.
pub fn browser_request(url: &str, web_root: &Path, profile: &Path) -> Value {
    json!({
        "type": "pwa-chrome",
        "request": "launch",
        "name": "Browser",
        "url": url,
        "webRoot": web_root,
        // A profile of its own, so the user's browser is never touched. Kept
        // and reused: with `true`, js-debug run on its own makes a new one in
        // the temp folder per run (50 MB and more) and never removes it.
        "userDataDir": profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_scripts_files_and_next_apps() {
        let root = db::testing::dir("debug-launch");
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"build":"next build","dev":"next dev -p 4000","test":"vitest"},"dependencies":{"next":"16"}}"#,
        )
        .unwrap();
        std::fs::write(root.join("pnpm-lock.yaml"), "").unwrap();
        std::fs::create_dir_all(root.join("api")).unwrap();
        std::fs::write(
            root.join("api/package.json"),
            r#"{"scripts":{"start":"node server.js"}}"#,
        )
        .unwrap();
        let file = root.join("scripts/seed.ts");
        let configs = detect(&root, Some(&file));
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "node scripts/seed.ts",
                "api: pnpm run start",
                "pnpm run dev + browser",
                "pnpm run dev",
                "pnpm run test",
                "pnpm run build",
            ]
        );
        assert_eq!(configs[0].request["program"], file.display().to_string());
        let dev = &configs[2];
        assert_eq!(dev.browser.as_deref(), Some("http://localhost:4000"));
        assert_eq!(dev.request["runtimeExecutable"], "pnpm");
        assert_eq!(dev.request["runtimeArgs"], json!(["run", "dev"]));
        assert_eq!(configs[1].request["cwd"], json!(root.join("api")));
        assert!(
            detect(&root, Some(Path::new("README.md")))
                .iter()
                .all(|c| !c.name.starts_with("node "))
        );
        assert_eq!(script_port("next dev --port=5000", 3000), 5000);
        assert_eq!(script_port("next dev", 3000), 3000);
        let browser = browser_request("http://localhost:3000", &root, &root.join("profile"));
        assert_eq!(browser["type"], "pwa-chrome");
        assert_eq!(browser["userDataDir"], json!(root.join("profile")));
    }
}
