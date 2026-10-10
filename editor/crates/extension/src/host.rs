//! Runs the code of a Zed extension: a WebAssembly component that says how
//! to get and start a language server.
//!
//! The component runs in wasmtime with a memory limit and a fuel budget for
//! each call. Its files are one folder of its own. Everything else it can do
//! goes through [`World`]: asking npm or GitHub for a version, downloading,
//! running a command its manifest declared. The app gives the real world
//! ([`crate::world::System`]); tests give a scripted one.
//!
//! Every call blocks, some for as long as a download takes. Call from a
//! thread that may wait, never from the UI thread or a Tokio worker.

use std::{
    collections::VecDeque,
    path::{Component as PathPart, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
    component::{Component, Resource, ResourceTable},
};
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::{Code, Extension};
/// An extension's own memory. They are small programs; this is generous.
const MEMORY: usize = 256 * 1024 * 1024;
/// What one call may compute before it is stopped. Waiting on the world
/// (a download, npm) costs none.
const FUEL: u64 = 20_000_000_000;
/// The same for painting the labels of a menu, which is waited for with
/// every completion: a fraction of a second at most.
const LABEL_FUEL: u64 = 500_000_000;

/// A program to start, as an extension describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// A completion as a language server sent it, for the extension to say how
/// to show it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub detail: Option<String>,
    /// The two parts of the item's `labelDetails`.
    pub label_detail: Option<String>,
    pub label_description: Option<String>,
    /// The protocol's numbers for the kind and the insert text format.
    pub kind: Option<i32>,
    pub format: Option<i32>,
}

/// A symbol a language server named: a function, a type, a file. `kind`
/// is the protocol's number for which.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: i32,
}

/// How an extension wants a completion or a symbol shown: a piece of code
/// in its language, to be highlighted as code, and the parts of it to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLabel {
    pub code: String,
    pub spans: Vec<LabelSpan>,
    /// The part of what is shown that the typed word is matched against.
    pub filter: std::ops::Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LabelSpan {
    /// A range of `code`.
    Code(std::ops::Range<usize>),
    /// Text that is not in `code`, colored as the named highlight.
    Literal {
        text: String,
        highlight: Option<String>,
    },
}

/// A program to debug, as the editor knows it before any adapter does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugLaunch {
    pub label: String,
    /// The debug adapter's name in its extension's manifest.
    pub adapter: String,
    pub program: String,
    pub cwd: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// How to start a debug adapter and what to ask it for, as an extension
/// worked it out from a [`DebugLaunch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugAdapter {
    /// The adapter's program. `None` for one that is not a process to
    /// start: it is only connected to.
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
    /// Where the adapter listens: an address, a port, and how long to wait
    /// for it in milliseconds. `None` means it talks on its own input and
    /// output.
    pub connection: Option<(std::net::Ipv4Addr, u16, Option<u64>)>,
    /// `false` to launch the program, `true` to attach to one that runs.
    pub attach: bool,
    /// The arguments of that request, as JSON.
    pub configuration: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// Name and download address of each file.
    pub assets: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Gzip,
    GzipTar,
    Zip,
    Uncompressed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// How many redirects to follow; `None` is as many as it takes.
    pub redirects: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpResponse {
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// What an extension says about the server it is getting ready.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    CheckingForUpdate,
    Downloading,
    Failed(String),
}

/// Everything an extension reaches outside its sandbox through.
pub trait World: Send + Sync + 'static {
    /// The path of Node.js: the user's, or one Solder got earlier.
    fn node(&self) -> Result<String, String>;
    /// Gets a Node.js of Solder's own for a machine that has none, and
    /// gives its path. A world that cannot says so.
    fn install_node(&self) -> Result<String, String> {
        Err("Node.js was not found, and this extension needs it".into())
    }
    fn npm_latest(&self, package: &str) -> Result<String, String>;
    /// Installs a package under `dir/node_modules`.
    fn npm_install(&self, dir: &Path, package: &str, version: &str) -> Result<(), String>;
    /// The release of a GitHub repository with this tag, or its latest.
    fn release(&self, repo: &str, tag: Option<&str>, pre_release: bool) -> Result<Release, String>;
    /// Downloads `url` to `dest`: a file, or a folder for an archive.
    fn download(&self, url: &str, dest: &Path, kind: FileKind) -> Result<(), String>;
    fn fetch(&self, request: HttpRequest) -> Result<HttpResponse, String>;
    /// Runs a command the extension's manifest declared.
    fn run(&self, command: &Command) -> Result<Output, String>;
    /// The full path of a program on the user's PATH.
    fn which(&self, binary: &str) -> Option<String>;
    /// The environment programs of the project start with.
    fn env(&self) -> Vec<(String, String)>;
    fn status(&self, server: &str, status: Status);
    /// The user's settings as an extension asks for them: for a language
    /// server (`lsp`, by the name the extension knows it under) or a
    /// language (`language`, by its name), in the JSON Zed's API gives.
    /// `None` when the user set nothing: the extension then sees what Zed
    /// gives in that case.
    fn settings(&self, _category: &str, _key: Option<&str>) -> Option<String> {
        None
    }
    /// The extension tried to run a command its manifest does not declare,
    /// and was not let. For the record of what it did; nothing to answer.
    fn undeclared(&self, _command: &Command) {}
}

/// Adds `more` to `into`, the way one extension's options are added to a
/// server's own: objects merge key by key, lists grow, and anything else is
/// replaced.
pub fn merge_json(into: &mut serde_json::Value, more: serde_json::Value) {
    use serde_json::Value;
    match (into, more) {
        (Value::Object(into), Value::Object(more)) => {
            for (key, value) in more {
                match into.get_mut(&key) {
                    Some(existing) => merge_json(existing, value),
                    None => {
                        into.insert(key, value);
                    }
                }
            }
        }
        (Value::Array(into), Value::Array(more)) => into.extend(more),
        (into, more) => *into = more,
    }
}

/// A project folder as an extension sees it.
pub struct Worktree {
    root: PathBuf,
}

pub struct Project;
pub struct KeyValueStore;

/// A response an extension reads piece by piece. It is read whole first:
/// what extensions fetch this way is small.
pub struct ResponseStream {
    chunks: VecDeque<Vec<u8>>,
}

struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    world: Arc<dyn World>,
    /// The extension's own folder, the only one it may write to.
    work_dir: PathBuf,
    /// The commands its manifest declared it runs.
    commands: Vec<(String, Vec<String>)>,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// `path` without `.` and `..`, resolved by name only.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            PathPart::CurDir => {}
            PathPart::ParentDir => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// Where an extension may write `path`: inside its own folder.
fn writable(work_dir: &Path, path: &str) -> Result<PathBuf, String> {
    let path = normalize(&work_dir.join(path));
    if path.starts_with(work_dir) {
        Ok(path)
    } else {
        Err(format!(
            "{} is outside the extension's folder",
            path.display()
        ))
    }
}

/// The program an extension named, as a path the system can start. A path
/// relative to the extension's folder (a server it downloaded there) is
/// made whole; a bare name is left for the PATH, and a full path as it is.
fn program(work_dir: &Path, command: &str) -> String {
    let path = Path::new(command);
    if path.is_absolute() || (path.components().count() < 2 && !work_dir.join(path).is_file()) {
        return command.to_string();
    }
    work_dir.join(path).to_string_lossy().into_owned()
}

/// Whether one of the declared `commands` covers `command`.
fn declared(commands: &[(String, Vec<String>)], command: &Command) -> bool {
    commands.iter().any(|(program, args)| {
        if program != "*" && *program != command.command {
            return false;
        }
        // `*` stands for one argument, `**` for all that remain.
        let mut given = command.args.iter();
        for pattern in args {
            match (pattern.as_str(), given.next()) {
                ("**", _) => return true,
                ("*", Some(_)) => {}
                (pattern, Some(arg)) if pattern == arg => {}
                _ => return false,
            }
        }
        given.next().is_none()
    })
}

// ------------------------------------------- what every version asks, once

/// What an extension is told the settings are when the user set none: what
/// Zed gives in that case.
fn unset(category: &str) -> Result<String, String> {
    match category {
        "language" => Ok(r#"{"tab_size":4}"#.into()),
        "lsp" => Ok(r#"{"binary":null,"initialization_options":null,"settings":null}"#.into()),
        "context_servers" => Ok(r#"{"command":null,"settings":null}"#.into()),
        other => Err(format!("Unknown settings category: {other}")),
    }
}

impl State {
    fn npm_installed(&self, package: &str) -> Result<Option<String>, String> {
        let manifest = self
            .work_dir
            .join("node_modules")
            .join(package)
            .join("package.json");
        let Ok(text) = std::fs::read_to_string(manifest) else {
            return Ok(None);
        };
        let manifest: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(manifest["version"].as_str().map(str::to_string))
    }

    fn latest_release(
        &self,
        repo: &str,
        require_assets: bool,
        pre_release: bool,
    ) -> Result<Release, String> {
        let release = self.world.release(repo, None, pre_release)?;
        if require_assets && release.assets.is_empty() {
            return Err(format!("The latest release of {repo} has no files"));
        }
        Ok(release)
    }

    fn stream(&mut self, request: HttpRequest) -> Result<Resource<ResponseStream>, String> {
        let response = self.world.fetch(request)?;
        let chunks = response
            .body
            .chunks(64 * 1024)
            .map(<[u8]>::to_vec)
            .collect();
        self.table
            .push(ResponseStream { chunks })
            .map_err(|e| e.to_string())
    }

    fn next_piece(&mut self, stream: Resource<ResponseStream>) -> Result<Option<Vec<u8>>, String> {
        let stream = self.table.get_mut(&stream).map_err(|e| e.to_string())?;
        Ok(stream.chunks.pop_front())
    }

    fn run_declared(&self, command: Command) -> Result<Output, String> {
        if !declared(&self.commands, &command) {
            self.world.undeclared(&command);
            return Err(format!(
                "The extension's manifest does not declare that it runs {}",
                command.command
            ));
        }
        self.world.run(&command)
    }

    fn download(&self, url: &str, path: &str, kind: FileKind) -> Result<(), String> {
        self.world
            .download(url, &writable(&self.work_dir, path)?, kind)
    }

    fn make_executable(&self, path: &str) -> Result<(), String> {
        let path = writable(&self.work_dir, path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .permissions();
            permissions.set_mode(permissions.mode() | 0o111);
            std::fs::set_permissions(&path, permissions).map_err(|e| e.to_string())?;
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    }

    fn worktree_root(&self, worktree: &Resource<Worktree>) -> String {
        self.table
            .get(worktree)
            .map(|w| w.root.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn worktree_file(&self, worktree: &Resource<Worktree>, path: &str) -> Result<String, String> {
        let root = &self.table.get(worktree).map_err(|e| e.to_string())?.root;
        let file = normalize(&root.join(path));
        if !file.starts_with(root) {
            return Err(format!("{path} is outside the project"));
        }
        std::fs::read_to_string(file).map_err(|e| format!("{path}: {e}"))
    }
}

/// What a version of the API exports, in the terms the host works in.
trait Calls: Send {
    /// How to start a server. `language` is the first language the manifest
    /// lists for it: the oldest versions are asked by name and language.
    fn command(
        &self,
        store: &mut Store<State>,
        server: &str,
        language: &str,
        worktree: Resource<Worktree>,
    ) -> wasmtime::Result<Result<Command, String>>;

    fn initialization_options(
        &self,
        store: &mut Store<State>,
        server: &str,
        language: &str,
        worktree: Resource<Worktree>,
    ) -> wasmtime::Result<Result<Option<String>, String>>;

    fn workspace_configuration(
        &self,
        store: &mut Store<State>,
        server: &str,
        worktree: Resource<Worktree>,
    ) -> wasmtime::Result<Result<Option<String>, String>>;

    /// What this extension's `server` adds to the initialization options of
    /// `target`, a server that is not its own. Versions before 0.4 cannot
    /// say, and add nothing.
    fn additional_initialization_options(
        &self,
        _store: &mut Store<State>,
        _server: &str,
        _target: &str,
        _worktree: Resource<Worktree>,
    ) -> wasmtime::Result<Result<Option<String>, String>> {
        Ok(Ok(None))
    }

    /// The same for the settings `target` is given.
    fn additional_workspace_configuration(
        &self,
        _store: &mut Store<State>,
        _server: &str,
        _target: &str,
        _worktree: Resource<Worktree>,
    ) -> wasmtime::Result<Result<Option<String>, String>> {
        Ok(Ok(None))
    }

    /// How to start the adapter that debugs `launch`. Versions before 0.6
    /// know no debug adapters.
    fn debug_adapter(
        &self,
        _store: &mut Store<State>,
        _launch: &DebugLaunch,
        _worktree: Resource<Worktree>,
    ) -> wasmtime::Result<Result<DebugAdapter, String>> {
        Ok(Err(
            "It was built for a version of Zed's API that has no debug adapters".into(),
        ))
    }

    /// How to start the context server `server` of this extension. Versions
    /// before 0.2 know none.
    fn context_server_command(
        &self,
        _store: &mut Store<State>,
        _server: &str,
        _project: Resource<Project>,
    ) -> wasmtime::Result<Result<Command, String>> {
        Ok(Err(
            "It was built for a version of Zed's API that has no context servers".into(),
        ))
    }

    /// How to show each of a server's completions; `None` leaves one as
    /// the server sent it. Versions before 0.0.6 cannot say.
    fn labels_for_completions(
        &self,
        _store: &mut Store<State>,
        _server: &str,
        completions: &[Completion],
    ) -> wasmtime::Result<Result<Vec<Option<CodeLabel>>, String>> {
        Ok(Ok(vec![None; completions.len()]))
    }

    /// The same for the symbols a server lists.
    fn labels_for_symbols(
        &self,
        _store: &mut Store<State>,
        _server: &str,
        symbols: &[Symbol],
    ) -> wasmtime::Result<Result<Vec<Option<CodeLabel>>, String>> {
        Ok(Ok(vec![None; symbols.len()]))
    }
}

// --------------------------------------------- one world for each version
//
// Zed's API grew by a step at a time, and each step is a world of its own:
// a component built for one imports exactly its functions. The modules in
// `host/` generate the bindings of each from its WIT files; the macros here
// write the glue between those bindings and the code above, which is the
// same for every version that has the interface at all.

/// Where an interface only brings types, there is nothing to implement.
macro_rules! types_only {
    ($api:ident: $($interface:ident),+) => {
        $(impl $api::$interface::Host for State {})+
    };
}

macro_rules! platform {
    ($api:ident) => {{
        use $api::platform::{Architecture, Os};
        (
            match std::env::consts::OS {
                "macos" => Os::Mac,
                "windows" => Os::Windows,
                _ => Os::Linux,
            },
            match std::env::consts::ARCH {
                "aarch64" => Architecture::Aarch64,
                "x86" => Architecture::X86,
                _ => Architecture::X8664,
            },
        )
    }};
}

macro_rules! github_release {
    ($api:ident, $release:expr) => {{
        let release: Release = $release;
        $api::github::GithubRelease {
            version: release.version,
            assets: release
                .assets
                .into_iter()
                .map(|(name, download_url)| $api::github::GithubReleaseAsset { name, download_url })
                .collect(),
        }
    }};
}

macro_rules! file_kind {
    ($bindings:ident, $kind:expr) => {
        match $kind {
            $bindings::DownloadedFileType::Gzip => FileKind::Gzip,
            $bindings::DownloadedFileType::GzipTar => FileKind::GzipTar,
            $bindings::DownloadedFileType::Zip => FileKind::Zip,
            $bindings::DownloadedFileType::Uncompressed => FileKind::Uncompressed,
        }
    };
}

/// The `platform` and `nodejs` interfaces, and `github` with or without
/// releases by tag.
macro_rules! tools {
    ($api:ident) => {
        tools!(@common $api);
        impl $api::github::Host for State {
            tools!(@latest $api);
        }
    };
    ($api:ident, releases_by_tag) => {
        tools!(@common $api);
        impl $api::github::Host for State {
            tools!(@latest $api);

            fn github_release_by_tag_name(
                &mut self,
                repo: String,
                tag: String,
            ) -> Result<$api::github::GithubRelease, String> {
                Ok(github_release!(
                    $api,
                    self.world.release(&repo, Some(&tag), true)?
                ))
            }
        }
    };
    (@latest $api:ident) => {
        fn latest_github_release(
            &mut self,
            repo: String,
            options: $api::github::GithubReleaseOptions,
        ) -> Result<$api::github::GithubRelease, String> {
            Ok(github_release!(
                $api,
                self.latest_release(&repo, options.require_assets, options.pre_release)?
            ))
        }
    };
    (@common $api:ident) => {
        impl $api::platform::Host for State {
            fn current_platform(&mut self) -> ($api::platform::Os, $api::platform::Architecture) {
                platform!($api)
            }
        }

        impl $api::nodejs::Host for State {
            fn node_binary_path(&mut self) -> Result<String, String> {
                self.world.node()
            }

            fn npm_package_latest_version(&mut self, package: String) -> Result<String, String> {
                self.world.npm_latest(&package)
            }

            fn npm_package_installed_version(
                &mut self,
                package: String,
            ) -> Result<Option<String>, String> {
                self.npm_installed(&package)
            }

            fn npm_install_package(&mut self, package: String, version: String) -> Result<(), String> {
                self.world.npm_install(&self.work_dir, &package, &version)
            }
        }
    };
}

macro_rules! http {
    ($api:ident) => {
        fn request(request: $api::http_client::HttpRequest) -> HttpRequest {
            use $api::http_client::{HttpMethod, RedirectPolicy};
            HttpRequest {
                method: match request.method {
                    HttpMethod::Get => "GET",
                    HttpMethod::Head => "HEAD",
                    HttpMethod::Post => "POST",
                    HttpMethod::Put => "PUT",
                    HttpMethod::Delete => "DELETE",
                    HttpMethod::Options => "OPTIONS",
                    HttpMethod::Patch => "PATCH",
                },
                url: request.url,
                headers: request.headers,
                body: request.body,
                redirects: match request.redirect_policy {
                    RedirectPolicy::NoFollow => Some(0),
                    RedirectPolicy::FollowLimit(limit) => Some(limit),
                    RedirectPolicy::FollowAll => None,
                },
            }
        }

        impl $api::http_client::Host for State {
            fn fetch(
                &mut self,
                asked: $api::http_client::HttpRequest,
            ) -> Result<$api::http_client::HttpResponse, String> {
                let response = self.world.fetch(request(asked))?;
                Ok($api::http_client::HttpResponse {
                    headers: response.headers,
                    body: response.body,
                })
            }

            fn fetch_stream(
                &mut self,
                asked: $api::http_client::HttpRequest,
            ) -> Result<Resource<ResponseStream>, String> {
                self.stream(request(asked))
            }
        }

        impl $api::http_client::HostHttpResponseStream for State {
            fn next_chunk(
                &mut self,
                stream: Resource<ResponseStream>,
            ) -> Result<Option<Vec<u8>>, String> {
                self.next_piece(stream)
            }

            fn drop(&mut self, stream: Resource<ResponseStream>) -> wasmtime::Result<()> {
                self.table.delete(stream)?;
                Ok(())
            }
        }
    };
}

macro_rules! process {
    ($api:ident) => {
        impl $api::process::Host for State {
            fn run_command(
                &mut self,
                command: $api::process::Command,
            ) -> Result<$api::process::Output, String> {
                let output = self.run_declared(Command {
                    command: command.command,
                    args: command.args,
                    env: command.env,
                })?;
                Ok($api::process::Output {
                    status: output.status,
                    stdout: output.stdout,
                    stderr: output.stderr,
                })
            }
        }
    };
}

/// The project folder. From 0.0.6 it also has an id and says its path.
macro_rules! worktree {
    ($bindings:ident $(, $located:ident)?) => {
        impl $bindings::HostWorktree for State {
            $(
                fn id(&mut self, _: Resource<Worktree>) -> u64 {
                    let $located = 0;
                    $located
                }

                fn root_path(&mut self, worktree: Resource<Worktree>) -> String {
                    self.worktree_root(&worktree)
                }
            )?

            fn read_text_file(
                &mut self,
                worktree: Resource<Worktree>,
                path: String,
            ) -> Result<String, String> {
                self.worktree_file(&worktree, &path)
            }

            fn which(&mut self, _: Resource<Worktree>, binary: String) -> Option<String> {
                self.world.which(&binary)
            }

            fn shell_env(&mut self, _: Resource<Worktree>) -> Vec<(String, String)> {
                self.world.env()
            }

            fn drop(&mut self, worktree: Resource<Worktree>) -> wasmtime::Result<()> {
                self.table.delete(worktree)?;
                Ok(())
            }
        }
    };
}

macro_rules! key_value_store {
    ($bindings:ident) => {
        impl $bindings::HostKeyValueStore for State {
            fn insert(
                &mut self,
                _: Resource<KeyValueStore>,
                _: String,
                _: String,
            ) -> Result<(), String> {
                Err("Solder does not index documentation".into())
            }

            fn drop(&mut self, store: Resource<KeyValueStore>) -> wasmtime::Result<()> {
                self.table.delete(store)?;
                Ok(())
            }
        }
    };
}

macro_rules! project {
    ($bindings:ident) => {
        impl $bindings::HostProject for State {
            fn worktree_ids(&mut self, _: Resource<Project>) -> Vec<u64> {
                vec![0]
            }

            fn drop(&mut self, project: Resource<Project>) -> wasmtime::Result<()> {
                self.table.delete(project)?;
                Ok(())
            }
        }
    };
}

/// The functions of the world itself, as they are from 0.0.6 on.
macro_rules! world_functions {
    ($bindings:ident) => {
        impl $bindings::ExtensionImports for State {
            fn get_settings(
                &mut self,
                _: Option<$bindings::SettingsLocation>,
                category: String,
                key: Option<String>,
            ) -> Result<String, String> {
                self.world
                    .settings(&category, key.as_deref())
                    .map_or_else(|| unset(&category), Ok)
            }

            fn download_file(
                &mut self,
                url: String,
                path: String,
                kind: $bindings::DownloadedFileType,
            ) -> Result<(), String> {
                self.download(&url, &path, file_kind!($bindings, kind))
            }

            fn make_file_executable(&mut self, path: String) -> Result<(), String> {
                self.make_executable(&path)
            }

            fn set_language_server_installation_status(
                &mut self,
                server: String,
                status: $bindings::LanguageServerInstallationStatus,
            ) {
                use $bindings::LanguageServerInstallationStatus as S;
                self.world.status(
                    &server,
                    match status {
                        S::None => Status::Ready,
                        S::Downloading => Status::Downloading,
                        S::CheckingForUpdate => Status::CheckingForUpdate,
                        S::Failed(why) => Status::Failed(why),
                    },
                );
            }
        }
    };
}

/// Links a version's imports, starts the component and wraps its exports.
/// The oldest versions are asked about a server by its name and language
/// (`by_config`), the rest by its id.
macro_rules! start {
    ($bindings:ident) => {
        start!(@start $bindings);
        start!(@by_id $bindings {
            start!(@labels $bindings, |c| Some(
                $bindings::zed::extension::lsp::CompletionLabelDetails {
                    detail: c.label_detail.clone(),
                    description: c.label_description.clone(),
                }
            )
            .filter(|d| d.detail.is_some() || d.description.is_some()));
            start!(@context $bindings);
        });
    };
    // 0.0.6 and 0.1.0: a completion had no label details yet.
    ($bindings:ident, before_label_details) => {
        start!(@start $bindings);
        start!(@by_id $bindings {
            start!(@labels $bindings);
        });
    };
    // From 0.4, an extension may add to the options and settings of a
    // server that is not its own.
    ($bindings:ident, sets_up_others) => {
        start!($bindings, sets_up_others, {});
    };
    // From 0.6, what a version adds of its own is written where its
    // bindings are.
    ($bindings:ident, sets_up_others, { $($extra:tt)* }) => {
        start!(@start $bindings);
        start!(@by_id $bindings {
            start!(@labels $bindings, |c| Some(
                $bindings::zed::extension::lsp::CompletionLabelDetails {
                    detail: c.label_detail.clone(),
                    description: c.label_description.clone(),
                }
            )
            .filter(|d| d.detail.is_some() || d.description.is_some()));
            start!(@context $bindings);

            fn additional_initialization_options(
                &self,
                store: &mut Store<State>,
                server: &str,
                target: &str,
                worktree: Resource<Worktree>,
            ) -> wasmtime::Result<Result<Option<String>, String>> {
                self.call_language_server_additional_initialization_options(
                    store, server, target, worktree,
                )
            }

            fn additional_workspace_configuration(
                &self,
                store: &mut Store<State>,
                server: &str,
                target: &str,
                worktree: Resource<Worktree>,
            ) -> wasmtime::Result<Result<Option<String>, String>> {
                self.call_language_server_additional_workspace_configuration(
                    store, server, target, worktree,
                )
            }

            $($extra)*
        });
    };
    // From 0.2, an extension may bring a context server: it says how to
    // start it, for a project it is handed.
    (@context $bindings:ident) => {
        fn context_server_command(
            &self,
            store: &mut Store<State>,
            server: &str,
            project: Resource<Project>,
        ) -> wasmtime::Result<Result<Command, String>> {
            Ok(self
                .call_context_server_command(store, server, project)?
                .map(|command| Command {
                    command: command.command,
                    args: command.args,
                    env: command.env,
                }))
        }
    };
    (@labels $bindings:ident $(, |$c:ident| $details:expr)?) => {
        fn labels_for_completions(
            &self,
            store: &mut Store<State>,
            server: &str,
            completions: &[Completion],
        ) -> wasmtime::Result<Result<Vec<Option<CodeLabel>>, String>> {
            use $bindings::zed::extension::lsp;
            let completions: Vec<lsp::Completion> = completions
                .iter()
                .map(|c| lsp::Completion {
                    label: c.label.clone(),
                    $(label_details: {
                        let $c = c;
                        $details
                    },)?
                    detail: c.detail.clone(),
                    kind: c.kind.map(|kind| {
                        use lsp::CompletionKind as K;
                        match kind {
                            1 => K::Text,
                            2 => K::Method,
                            3 => K::Function,
                            4 => K::Constructor,
                            5 => K::Field,
                            6 => K::Variable,
                            7 => K::Class,
                            8 => K::Interface,
                            9 => K::Module,
                            10 => K::Property,
                            11 => K::Unit,
                            12 => K::Value,
                            13 => K::Enum,
                            14 => K::Keyword,
                            15 => K::Snippet,
                            16 => K::Color,
                            17 => K::File,
                            18 => K::Reference,
                            19 => K::Folder,
                            20 => K::EnumMember,
                            21 => K::Constant,
                            22 => K::Struct,
                            23 => K::Event,
                            24 => K::Operator,
                            25 => K::TypeParameter,
                            other => K::Other(other),
                        }
                    }),
                    insert_text_format: c.format.map(|format| match format {
                        1 => lsp::InsertTextFormat::PlainText,
                        2 => lsp::InsertTextFormat::Snippet,
                        other => lsp::InsertTextFormat::Other(other),
                    }),
                })
                .collect();
            let labels = self.call_labels_for_completions(store, server, &completions)?;
            Ok(labels.map(|labels| {
                labels
                    .into_iter()
                    .map(|label| label.map(|label| start!(@label $bindings, label)))
                    .collect()
            }))
        }

        fn labels_for_symbols(
            &self,
            store: &mut Store<State>,
            server: &str,
            symbols: &[Symbol],
        ) -> wasmtime::Result<Result<Vec<Option<CodeLabel>>, String>> {
            use $bindings::zed::extension::lsp;
            let symbols: Vec<lsp::Symbol> = symbols
                .iter()
                .map(|symbol| lsp::Symbol {
                    name: symbol.name.clone(),
                    kind: {
                        use lsp::SymbolKind as K;
                        match symbol.kind {
                            1 => K::File,
                            2 => K::Module,
                            3 => K::Namespace,
                            4 => K::Package,
                            5 => K::Class,
                            6 => K::Method,
                            7 => K::Property,
                            8 => K::Field,
                            9 => K::Constructor,
                            10 => K::Enum,
                            11 => K::Interface,
                            12 => K::Function,
                            13 => K::Variable,
                            14 => K::Constant,
                            15 => K::String,
                            16 => K::Number,
                            17 => K::Boolean,
                            18 => K::Array,
                            19 => K::Object,
                            20 => K::Key,
                            21 => K::Null,
                            22 => K::EnumMember,
                            23 => K::Struct,
                            24 => K::Event,
                            25 => K::Operator,
                            26 => K::TypeParameter,
                            other => K::Other(other),
                        }
                    },
                })
                .collect();
            let labels = self.call_labels_for_symbols(store, server, &symbols)?;
            Ok(labels.map(|labels| {
                labels
                    .into_iter()
                    .map(|label| label.map(|label| start!(@label $bindings, label)))
                    .collect()
            }))
        }
    };
    // A label as the extension wrote it, in the editor's own shape.
    (@label $bindings:ident, $label:expr) => {{
        let label = $label;
        CodeLabel {
            code: label.code,
            spans: label
                .spans
                .into_iter()
                .map(|span| match span {
                    $bindings::CodeLabelSpan::CodeRange(range) => {
                        LabelSpan::Code(range.start as usize..range.end as usize)
                    }
                    $bindings::CodeLabelSpan::Literal(literal) => LabelSpan::Literal {
                        text: literal.text,
                        highlight: literal.highlight_name,
                    },
                })
                .collect(),
            filter: label.filter_range.start as usize..label.filter_range.end as usize,
        }
    }};
    (@by_id $bindings:ident { $($more:tt)* }) => {
        impl Calls for $bindings::Extension {
            fn command(
                &self,
                store: &mut Store<State>,
                server: &str,
                _language: &str,
                worktree: Resource<Worktree>,
            ) -> wasmtime::Result<Result<Command, String>> {
                Ok(self
                    .call_language_server_command(store, server, worktree)?
                    .map(|command| Command {
                        command: command.command,
                        args: command.args,
                        env: command.env,
                    }))
            }

            fn initialization_options(
                &self,
                store: &mut Store<State>,
                server: &str,
                _language: &str,
                worktree: Resource<Worktree>,
            ) -> wasmtime::Result<Result<Option<String>, String>> {
                self.call_language_server_initialization_options(store, server, worktree)
            }

            fn workspace_configuration(
                &self,
                store: &mut Store<State>,
                server: &str,
                worktree: Resource<Worktree>,
            ) -> wasmtime::Result<Result<Option<String>, String>> {
                self.call_language_server_workspace_configuration(store, server, worktree)
            }

            $($more)*
        }
    };
    ($bindings:ident, by_config) => {
        start!(@start $bindings);

        impl Calls for $bindings::Extension {
            fn command(
                &self,
                store: &mut Store<State>,
                server: &str,
                language: &str,
                worktree: Resource<Worktree>,
            ) -> wasmtime::Result<Result<Command, String>> {
                let config = $bindings::LanguageServerConfig {
                    name: server.to_string(),
                    language_name: language.to_string(),
                };
                Ok(self
                    .call_language_server_command(store, &config, worktree)?
                    .map(|command| Command {
                        command: command.command,
                        args: command.args,
                        env: command.env,
                    }))
            }

            fn initialization_options(
                &self,
                store: &mut Store<State>,
                server: &str,
                language: &str,
                worktree: Resource<Worktree>,
            ) -> wasmtime::Result<Result<Option<String>, String>> {
                let config = $bindings::LanguageServerConfig {
                    name: server.to_string(),
                    language_name: language.to_string(),
                };
                self.call_language_server_initialization_options(store, &config, worktree)
            }

            /// These versions have no settings to give a server.
            fn workspace_configuration(
                &self,
                _: &mut Store<State>,
                _: &str,
                _: Resource<Worktree>,
            ) -> wasmtime::Result<Result<Option<String>, String>> {
                Ok(Ok(None))
            }
        }
    };
    (@start $bindings:ident) => {
        pub(super) fn start(
            store: &mut Store<State>,
            component: &wasmtime::component::Component,
        ) -> wasmtime::Result<Box<dyn Calls>> {
            let mut linker = wasmtime::component::Linker::new(store.engine());
            wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
            $bindings::Extension::add_to_linker::<State, wasmtime::component::HasSelf<State>>(
                &mut linker,
                |state| state,
            )?;
            let extension = $bindings::Extension::instantiate(&mut *store, component, &linker)?;
            extension.call_init_extension(&mut *store)?;
            Ok(Box::new(extension))
        }
    };
}

mod v0_0_1;
mod v0_0_4;
mod v0_0_6;
mod v0_1_0;
mod v0_2_0;
mod v0_3_0;
mod v0_4_0;
mod v0_5_0;
mod v0_6_0;

/// Starts a component with the world of the API version it was built for:
/// the newest one that is not newer than `api`.
type Start = fn(&mut Store<State>, &Component) -> wasmtime::Result<Box<dyn Calls>>;

/// The first version of each world, oldest first.
const WORLDS: &[((u32, u32, u32), Start)] = &[
    ((0, 0, 1), v0_0_1::start),
    ((0, 0, 4), v0_0_4::start),
    ((0, 0, 6), v0_0_6::start),
    ((0, 1, 0), v0_1_0::start),
    ((0, 2, 0), v0_2_0::start),
    ((0, 3, 0), v0_3_0::start),
    ((0, 4, 0), v0_4_0::start),
    ((0, 5, 0), v0_5_0::start),
    ((0, 6, 0), v0_6_0::start),
];
/// The first version this host does not know. 0.7 added nothing to the
/// world of 0.6.
const UNKNOWN: (u32, u32, u32) = (0, 8, 0);

/// The world for `api`, with the version it starts at.
fn world_for(api: &str) -> Option<&'static ((u32, u32, u32), Start)> {
    let mut parts = api.split('.').map(|p| p.parse::<u32>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    if version >= UNKNOWN {
        return None;
    }
    WORLDS.iter().rev().find(|(since, _)| version >= *since)
}

// ------------------------------------------------------------------ the host

fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.consume_fuel(true);
        Engine::new(&config).expect("the settings are fixed and valid")
    })
}

/// Whether this host runs extensions built for `api` (`0.7.0`).
pub fn runs(api: &str) -> bool {
    world_for(api).is_some()
}

struct Running {
    store: Store<State>,
    extension: Box<dyn Calls>,
}

/// One extension's code, loaded and ready to be asked.
pub struct Host {
    running: Mutex<Running>,
    /// The first language the manifest lists for each server.
    languages: Vec<(String, String)>,
    /// The debug adapters the manifest declares.
    debug_adapters: Vec<String>,
    context_servers: Vec<String>,
}

impl Host {
    /// Compiles and starts the code of `extension`, which keeps its files in
    /// `work_dir`. Slow: the compilation alone takes a noticeable moment.
    pub fn load(
        extension: &Extension,
        work_dir: &Path,
        world: Arc<dyn World>,
    ) -> Result<Self, String> {
        let Code::Zed { api } = &extension.code else {
            return Err("The extension has no code to run".into());
        };
        let Some((_, start)) = world_for(api) else {
            return Err(format!(
                "It was built for version {api} of Zed's extension API, which Solder does not run yet"
            ));
        };
        std::fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
        // Paths the extension builds from its folder must be the real ones.
        let work_dir = work_dir.canonicalize().map_err(|e| e.to_string())?;
        let bytes =
            std::fs::read(extension.dir.join("extension.wasm")).map_err(|e| e.to_string())?;
        let problem = |e: wasmtime::Error| format!("{e:#}");
        let component = Component::new(engine(), &bytes).map_err(problem)?;

        // The extension's folder is the whole of its file system. It is
        // there twice: as the current folder, and under its real path, which
        // is what the extension joins file names to when it answers.
        let here = work_dir.to_string_lossy().into_owned();
        let mut wasi = WasiCtxBuilder::new();
        wasi.env("PWD", &here);
        for guest in [".", here.as_str()] {
            wasi.preopened_dir(&work_dir, guest, DirPerms::all(), FilePerms::all())
                .map_err(problem)?;
        }
        let mut store = Store::new(
            engine(),
            State {
                wasi: wasi.build(),
                table: ResourceTable::new(),
                limits: StoreLimitsBuilder::new().memory_size(MEMORY).build(),
                world,
                work_dir,
                commands: extension.commands.clone(),
            },
        );
        store.limiter(|state| &mut state.limits);
        store.set_fuel(FUEL).map_err(problem)?;
        let running = start(&mut store, &component).map_err(problem)?;
        Ok(Self {
            running: Mutex::new(Running {
                store,
                extension: running,
            }),
            languages: extension
                .servers
                .iter()
                .map(|server| {
                    (
                        server.id.clone(),
                        server.languages.first().cloned().unwrap_or_default(),
                    )
                })
                .collect(),
            debug_adapters: extension.debug_adapters.clone(),
            context_servers: extension.context_servers.clone(),
        })
    }

    fn language(&self, server: &str) -> &str {
        self.languages
            .iter()
            .find(|(id, _)| id == server)
            .map_or("", |(_, language)| language)
    }

    /// Calls into the extension with a project folder in hand.
    fn ask<T>(
        &self,
        root: &Path,
        call: impl FnOnce(
            &dyn Calls,
            &mut Store<State>,
            Resource<Worktree>,
        ) -> wasmtime::Result<Result<T, String>>,
    ) -> Result<T, String> {
        let mut running = self.running.lock().unwrap();
        let Running { store, extension } = &mut *running;
        let problem = |e: wasmtime::Error| format!("The extension failed: {e:#}");
        store.set_fuel(FUEL).map_err(problem)?;
        let worktree = store
            .data_mut()
            .table
            .push(Worktree {
                root: root.to_path_buf(),
            })
            .map_err(|e| e.to_string())?;
        let rep = worktree.rep();
        let answer = call(&**extension, store, worktree);
        // The extension only borrowed the folder; the entry is ours to drop.
        let _ = store
            .data_mut()
            .table
            .delete(Resource::<Worktree>::new_own(rep));
        answer.map_err(problem)?
    }

    /// How to start the language server `server` for the project in `root`.
    /// The extension may download the server first.
    pub fn language_server_command(&self, server: &str, root: &Path) -> Result<Command, String> {
        let language = self.language(server);
        let command = self.ask(root, |extension, store, worktree| {
            extension.command(store, server, language, worktree)
        })?;
        let running = self.running.lock().unwrap();
        Ok(Command {
            command: program(&running.store.data().work_dir, &command.command),
            args: command.args,
            env: command.env,
        })
    }

    /// How the extension wants `completions` of its `server` shown, one
    /// answer for each. `None` when the extension is in the middle of
    /// something else, a download for one: a menu does not wait for that.
    pub fn labels_for_completions(
        &self,
        server: &str,
        completions: &[Completion],
    ) -> Option<Result<Vec<Option<CodeLabel>>, String>> {
        self.labels(completions.len(), |extension, store| {
            extension.labels_for_completions(store, server, completions)
        })
    }

    /// The same for the symbols its `server` lists.
    pub fn labels_for_symbols(
        &self,
        server: &str,
        symbols: &[Symbol],
    ) -> Option<Result<Vec<Option<CodeLabel>>, String>> {
        self.labels(symbols.len(), |extension, store| {
            extension.labels_for_symbols(store, server, symbols)
        })
    }

    /// Asks the extension for labels, one for each of `count` things.
    fn labels(
        &self,
        count: usize,
        ask: impl FnOnce(
            &dyn Calls,
            &mut Store<State>,
        ) -> wasmtime::Result<Result<Vec<Option<CodeLabel>>, String>>,
    ) -> Option<Result<Vec<Option<CodeLabel>>, String>> {
        let mut running = self.running.try_lock().ok()?;
        let Running { store, extension } = &mut *running;
        let problem = |e: wasmtime::Error| format!("The extension failed: {e:#}");
        if let Err(error) = store.set_fuel(LABEL_FUEL) {
            return Some(Err(problem(error)));
        }
        let labels = ask(&**extension, store)
            .map_err(problem)
            .and_then(|labels| labels);
        // An extension answers up to the last thing it has a label for:
        // the list may be shorter than what it was asked about.
        Some(labels.map(|mut labels| {
            labels.resize(count, None);
            labels
        }))
    }

    /// How to start the debug adapter for `launch` in the project at
    /// `root`. The extension may install the adapter first.
    pub fn debug_adapter(&self, launch: &DebugLaunch, root: &Path) -> Result<DebugAdapter, String> {
        // Asked about an adapter that is not its own, an extension may
        // answer all the same, with its own.
        if !self.debug_adapters.contains(&launch.adapter) {
            return Err(format!(
                "The extension has no debug adapter called {}",
                launch.adapter
            ));
        }
        let mut adapter = self.ask(root, |extension, store, worktree| {
            extension.debug_adapter(store, launch, worktree)
        })?;
        let running = self.running.lock().unwrap();
        adapter.command = adapter
            .command
            .map(|command| program(&running.store.data().work_dir, &command));
        Ok(adapter)
    }

    /// How to start the context server `server` this extension brings. It
    /// may install the server first, and reads the user's settings for it
    /// (a database's address, a token): without one it needs, it says so.
    pub fn context_server_command(&self, server: &str) -> Result<Command, String> {
        if !self.context_servers.iter().any(|known| known == server) {
            return Err(format!(
                "The extension has no context server called {server}"
            ));
        }
        let mut running = self.running.lock().unwrap();
        let Running { store, extension } = &mut *running;
        let problem = |e: wasmtime::Error| format!("The extension failed: {e:#}");
        store.set_fuel(FUEL).map_err(problem)?;
        let project = store
            .data_mut()
            .table
            .push(Project)
            .map_err(|e| e.to_string())?;
        let rep = project.rep();
        let answer = extension.context_server_command(store, server, project);
        // Borrowed for the call, as a worktree is.
        let _ = store
            .data_mut()
            .table
            .delete(Resource::<Project>::new_own(rep));
        let command = answer.map_err(problem)??;
        Ok(Command {
            command: program(&store.data().work_dir, &command.command),
            args: command.args,
            env: command.env,
        })
    }

    /// The JSON to send the server as `initializationOptions`, if any.
    pub fn initialization_options(
        &self,
        server: &str,
        root: &Path,
    ) -> Result<Option<String>, String> {
        let language = self.language(server);
        self.ask(root, |extension, store, worktree| {
            extension.initialization_options(store, server, language, worktree)
        })
    }

    /// The JSON to answer the server's `workspace/configuration` with.
    pub fn workspace_configuration(
        &self,
        server: &str,
        root: &Path,
    ) -> Result<Option<String>, String> {
        self.ask(root, |extension, store, worktree| {
            extension.workspace_configuration(store, server, worktree)
        })
    }

    /// The JSON this extension's `server` adds to the initialization options
    /// of `target`, a server it does not bring itself: Vue's adds its plugin
    /// to the TypeScript server's.
    pub fn additional_initialization_options(
        &self,
        server: &str,
        target: &str,
        root: &Path,
    ) -> Result<Option<String>, String> {
        self.ask(root, |extension, store, worktree| {
            extension.additional_initialization_options(store, server, target, worktree)
        })
    }

    /// The JSON this extension's `server` adds to the settings of `target`.
    pub fn additional_workspace_configuration(
        &self,
        server: &str,
        target: &str,
        root: &Path,
    ) -> Result<Option<String>, String> {
        self.ask(root, |extension, store, worktree| {
            extension.additional_workspace_configuration(store, server, target, worktree)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extension_writes_only_inside_its_folder() {
        let dir = Path::new("/data/work/html");
        assert_eq!(
            writable(dir, "server/bin/ls").unwrap(),
            dir.join("server/bin/ls")
        );
        assert_eq!(writable(dir, "./a/../b").unwrap(), dir.join("b"));
        assert_eq!(writable(dir, "/data/work/html/x").unwrap(), dir.join("x"));
        assert!(writable(dir, "../other/x").is_err());
        assert!(writable(dir, "a/../../../etc/passwd").is_err());
        assert!(writable(dir, "/etc/passwd").is_err());
        // A neighbour whose name starts the same is not inside.
        assert!(writable(dir, "/data/work/html-evil/x").is_err());
    }

    #[test]
    fn a_program_in_the_extensions_folder_gets_its_full_path() {
        let dir = crate::testing::scratch("host-program");
        crate::testing::write(&dir.join("server-v1/bin/server"), "");
        crate::testing::write(&dir.join("taplo"), "");
        let full = |relative: &str| dir.join(relative).to_string_lossy().into_owned();
        // What an extension answers after downloading a server.
        assert_eq!(
            program(&dir, "server-v1/bin/server"),
            full("server-v1/bin/server")
        );
        assert_eq!(
            program(&dir, "./server-v1/bin/server"),
            full("./server-v1/bin/server")
        );
        // A file directly in the folder, against a name meant for the PATH.
        assert_eq!(program(&dir, "taplo"), full("taplo"));
        assert_eq!(program(&dir, "gopls"), "gopls");
        assert_eq!(program(&dir, "/usr/bin/node"), "/usr/bin/node");
    }

    #[test]
    fn options_of_two_sources_merge() {
        use serde_json::json;
        let mut options = json!({
            "hostInfo": "solder",
            "plugins": [{ "name": "mine" }],
            "preferences": { "quotes": "single", "semi": true }
        });
        merge_json(
            &mut options,
            json!({
                "plugins": [{ "name": "@vue/typescript-plugin" }],
                "preferences": { "semi": false },
                "tsserver": { "logVerbosity": "off" }
            }),
        );
        assert_eq!(
            options,
            json!({
                "hostInfo": "solder",
                "plugins": [{ "name": "mine" }, { "name": "@vue/typescript-plugin" }],
                "preferences": { "quotes": "single", "semi": false },
                "tsserver": { "logVerbosity": "off" }
            })
        );
        // Nothing there yet: what is added is all there is.
        let mut empty = serde_json::Value::Null;
        merge_json(&mut empty, json!({ "a": 1 }));
        assert_eq!(empty, json!({ "a": 1 }));
    }

    #[test]
    fn a_command_runs_only_as_declared() {
        let command = |program: &str, args: &[&str]| Command {
            command: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: Vec::new(),
        };
        let grant = |program: &str, args: &[&str]| {
            vec![(
                program.to_string(),
                args.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            )]
        };
        let exact = grant("gem", &["install", "*"]);
        assert!(declared(
            &exact,
            &command("gem", &["install", "solargraph"])
        ));
        assert!(!declared(&exact, &command("gem", &["install"])));
        assert!(!declared(&exact, &command("gem", &["install", "a", "b"])));
        assert!(!declared(&exact, &command("gem", &["uninstall", "a"])));
        assert!(!declared(&exact, &command("rm", &["install", "a"])));
        let rest = grant("gem", &["install", "**"]);
        assert!(declared(&rest, &command("gem", &["install"])));
        assert!(declared(
            &rest,
            &command("gem", &["install", "a", "--user"])
        ));
        assert!(declared(
            &grant("*", &["--version"]),
            &command("any", &["--version"])
        ));
        assert!(!declared(&[], &command("ls", &[])));
    }

    #[test]
    fn the_api_versions_it_runs() {
        // Every version published so far, each with the newest world that
        // is not newer than it.
        let world = |api: &str| world_for(api).map(|(since, _)| *since);
        assert_eq!(world("0.0.1"), Some((0, 0, 1)));
        assert_eq!(world("0.0.3"), Some((0, 0, 1)));
        assert_eq!(world("0.0.4"), Some((0, 0, 4)));
        assert_eq!(world("0.0.6"), Some((0, 0, 6)));
        assert_eq!(world("0.0.7"), Some((0, 0, 6)));
        assert_eq!(world("0.1.0"), Some((0, 1, 0)));
        assert_eq!(world("0.2.0"), Some((0, 2, 0)));
        assert_eq!(world("0.3.0"), Some((0, 3, 0)));
        assert_eq!(world("0.4.0"), Some((0, 4, 0)));
        assert_eq!(world("0.5.0"), Some((0, 5, 0)));
        assert_eq!(world("0.6.0"), Some((0, 6, 0)));
        assert_eq!(world("0.7.3"), Some((0, 6, 0)));
        assert!(runs("0.7.0"));
        assert!(!runs("0.8.0"));
        assert!(!runs("1.0.0"));
        assert!(!runs("0.0.0"));
        assert!(!runs("0.6"));
        assert!(!runs(""));
    }
}
