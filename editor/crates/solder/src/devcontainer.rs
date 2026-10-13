//! A bounded devcontainer implementation, without a transport or UI.
//!
//! Parsing and planning do not run Docker. The parent obtains configuration
//! consent before calling a runner (including Compose configuration resolution),
//! and keeps blocking IO off the UI thread. Dockerfiles, Compose files and
//! lifecycle scripts are executable configuration, not a security sandbox.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_CONFIG: usize = 1024 * 1024;
const MAX_OUTPUT: usize = 8 * MAX_CONFIG;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(String),
    Unsupported(String),
    Io(String),
    Command {
        operation: Operation,
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "Invalid devcontainer configuration: {message}"),
            Self::Unsupported(message) => write!(f, "Unsupported devcontainer feature: {message}"),
            Self::Io(message) => write!(f, "Devcontainer IO failed: {message}"),
            Self::Command {
                operation,
                code,
                stdout,
                stderr,
            } => {
                write!(
                    f,
                    "Docker {operation:?} failed ({code:?}): {}",
                    if stderr.is_empty() { stdout } else { stderr }
                )
            }
        }
    }
}

impl std::error::Error for Error {}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

fn unsupported(message: impl Into<String>) -> Error {
    Error::Unsupported(message.into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Image(String),
    Dockerfile {
        dockerfile: PathBuf,
        context: PathBuf,
        args: BTreeMap<String, String>,
        target: Option<String>,
    },
    Compose {
        files: Vec<PathBuf>,
        service: String,
        /// Empty means all services, as in the specification.
        run_services: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceMount {
    pub source: PathBuf,
    pub target: String,
    pub readonly: bool,
    pub consistency: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleCommand {
    /// Executed literally by /bin/sh inside the container, after consent.
    Shell(String),
    Argv(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lifecycle {
    OnCreate,
    UpdateContent,
    PostCreate,
    PostStart,
    PostAttach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownAction {
    None,
    StopContainer,
    StopCompose,
}

#[derive(Clone, Debug)]
pub struct Config {
    file: PathBuf,
    workspace: PathBuf,
    name: Option<String>,
    source: Source,
    mount: Option<WorkspaceMount>,
    folder: String,
    container_env: BTreeMap<String, String>,
    remote_env: BTreeMap<String, String>,
    local_env: BTreeMap<String, String>,
    remote_user: Option<String>,
    container_user: Option<String>,
    run_args: Vec<String>,
    lifecycle: BTreeMap<Lifecycle, LifecycleCommand>,
    shutdown: ShutdownAction,
    override_command: bool,
    init: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawConfig {
    name: Option<String>,
    image: Option<String>,
    build: Option<RawBuild>,
    #[serde(rename = "dockerFile")]
    dockerfile: Option<String>,
    context: Option<String>,
    docker_compose_file: Option<Value>,
    service: Option<String>,
    #[serde(default)]
    run_services: Vec<String>,
    workspace_folder: Option<String>,
    workspace_mount: Option<String>,
    #[serde(default)]
    container_env: BTreeMap<String, String>,
    #[serde(default)]
    remote_env: BTreeMap<String, String>,
    remote_user: Option<String>,
    container_user: Option<String>,
    #[serde(default)]
    run_args: Vec<String>,
    initialize_command: Option<Value>,
    on_create_command: Option<Value>,
    update_content_command: Option<Value>,
    post_create_command: Option<Value>,
    post_start_command: Option<Value>,
    post_attach_command: Option<Value>,
    wait_for: Option<String>,
    shutdown_action: Option<String>,
    override_command: Option<bool>,
    #[serde(default)]
    init: bool,
    #[serde(rename = "updateRemoteUserUID")]
    update_remote_user_uid: Option<bool>,
    user_env_probe: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
    dockerfile: String,
    context: Option<String>,
    #[serde(default)]
    args: BTreeMap<String, String>,
    target: Option<String>,
}

/// Discover the two default locations. Named configurations must be selected by
/// the parent and passed to `Config::load`; they are never selected arbitrarily.
pub fn discover(workspace: &Path) -> Result<Option<PathBuf>> {
    let root = canonical(workspace)?;
    for relative in [".devcontainer/devcontainer.json", ".devcontainer.json"] {
        let candidate = root.join(relative);
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => return scoped_path(&root, &root, &candidate, false).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::Io(error.to_string())),
        }
    }
    Ok(None)
}

impl Config {
    /// `local_env` is a consented snapshot supplied by the parent. This module
    /// never reads the host environment implicitly for variable substitution.
    pub fn load(
        file: &Path,
        workspace: &Path,
        local_env: BTreeMap<String, String>,
    ) -> Result<Self> {
        let root = canonical(workspace)?;
        let file = scoped_path(&root, &root, file, false)?;
        let mut source = String::new();
        std::fs::File::open(&file)
            .and_then(|input| {
                input
                    .take((MAX_CONFIG + 1) as u64)
                    .read_to_string(&mut source)
            })
            .map_err(|error| Error::Io(error.to_string()))?;
        Self::parse(&source, &file, &root, local_env)
    }

    pub fn parse(
        source: &str,
        file: &Path,
        workspace: &Path,
        local_env: BTreeMap<String, String>,
    ) -> Result<Self> {
        if source.len() > MAX_CONFIG {
            return Err(invalid("configuration exceeds 1 MiB"));
        }
        let workspace = canonical(workspace)?;
        if !workspace.is_dir() {
            return Err(invalid("workspace must be a directory"));
        }
        let file = scoped_path(&workspace, &workspace, file, false)?;
        let base = file
            .parent()
            .ok_or_else(|| invalid("configuration has no parent folder"))?;
        let value = import::jsonc::parse(source).map_err(invalid)?;
        let object = value
            .as_object()
            .ok_or_else(|| invalid("configuration must be an object"))?;
        // Reject rather than silently ignore settings that would change runtime
        // behavior. Empty unsupported properties are still explicit requests.
        const SUPPORTED: &[&str] = &[
            "name",
            "image",
            "build",
            "dockerFile",
            "context",
            "dockerComposeFile",
            "service",
            "runServices",
            "workspaceFolder",
            "workspaceMount",
            "containerEnv",
            "remoteEnv",
            "remoteUser",
            "containerUser",
            "runArgs",
            "initializeCommand",
            "onCreateCommand",
            "updateContentCommand",
            "postCreateCommand",
            "postStartCommand",
            "postAttachCommand",
            "waitFor",
            "shutdownAction",
            "overrideCommand",
            "init",
            "updateRemoteUserUID",
            "userEnvProbe",
        ];
        for key in object.keys() {
            if !SUPPORTED.contains(&key.as_str()) {
                return Err(unsupported(key));
            }
        }
        if object.values().any(Value::is_null) {
            return Err(unsupported("null property values"));
        }
        if let Some(build) = object.get("build").and_then(Value::as_object) {
            allowed_keys(build, &["dockerfile", "context", "args", "target"], "build")?;
        }
        let raw: RawConfig =
            serde_json::from_value(value).map_err(|error| invalid(error.to_string()))?;
        if raw.initialize_command.is_some() {
            return Err(unsupported("initializeCommand executes on the host"));
        }
        if raw.update_remote_user_uid == Some(true) {
            return Err(unsupported("updateRemoteUserUID"));
        }
        if raw
            .user_env_probe
            .as_deref()
            .is_some_and(|probe| probe != "none")
        {
            return Err(unsupported("userEnvProbe other than none"));
        }
        if raw
            .wait_for
            .as_deref()
            .is_some_and(|wait| wait != "postAttachCommand")
        {
            return Err(unsupported(
                "asynchronous waitFor; only postAttachCommand is supported",
            ));
        }
        let compose = raw.docker_compose_file.is_some();
        let builds = usize::from(raw.build.is_some()) + usize::from(raw.dockerfile.is_some());
        if usize::from(raw.image.is_some()) + builds + usize::from(compose) != 1 {
            return Err(invalid(
                "choose exactly one of image, build, dockerFile, or dockerComposeFile",
            ));
        }
        if raw.context.is_some() && raw.dockerfile.is_none() {
            return Err(invalid(
                "context requires dockerFile; use build.context with build",
            ));
        }
        if compose {
            if raw.workspace_mount.is_some()
                || !raw.container_env.is_empty()
                || raw.container_user.is_some()
                || !raw.run_args.is_empty()
                || raw.init
                || raw.override_command == Some(true)
            {
                return Err(unsupported(
                    "Compose runtime overrides; put mounts, environment, user, init and command in the Compose service",
                ));
            }
        } else if raw.service.is_some() || !raw.run_services.is_empty() {
            return Err(invalid("service and runServices require dockerComposeFile"));
        }
        let basename = path_text(&workspace)?.rsplit('/').next().unwrap_or("");
        if basename.is_empty() {
            return Err(invalid("workspace needs a folder name"));
        }
        let default_target = if compose {
            "/".to_owned()
        } else {
            format!("/workspaces/{basename}")
        };
        let initial = Variables {
            workspace: &workspace,
            container_folder: &default_target,
            local_env: &local_env,
            container_env: None,
        };
        let mount = if compose {
            None
        } else if let Some(mount) = &raw.workspace_mount {
            if raw.workspace_folder.is_none() {
                return Err(invalid("workspaceMount requires workspaceFolder"));
            }
            Some(parse_mount(
                &substitute(mount, &initial)?,
                &workspace,
                base,
            )?)
        } else {
            Some(WorkspaceMount {
                source: workspace.clone(),
                target: default_target.clone(),
                readonly: false,
                consistency: None,
            })
        };
        let target = mount
            .as_ref()
            .map_or(default_target.as_str(), |mount| mount.target.as_str());
        let variables = Variables {
            container_folder: target,
            ..initial
        };
        let folder = substitute(
            raw.workspace_folder.as_deref().unwrap_or(target),
            &variables,
        )?;
        container_path(&folder)?;
        if let Some(mount) = &mount
            && !container_contains(&mount.target, &folder)
        {
            return Err(invalid("workspaceFolder must be inside workspaceMount"));
        }
        let source = if let Some(image) = raw.image {
            let image = substitute(&image, &variables)?;
            image_name(&image)?;
            Source::Image(image)
        } else if let Some(files) = raw.docker_compose_file {
            let files = strings(&files, "dockerComposeFile")?;
            if files.is_empty() {
                return Err(invalid("dockerComposeFile must not be empty"));
            }
            let files = files
                .iter()
                .map(|file| {
                    scoped_path(
                        &workspace,
                        base,
                        Path::new(&substitute(file, &variables)?),
                        false,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let service = raw
                .service
                .ok_or_else(|| invalid("Compose requires service"))?;
            let service = substitute(&service, &variables)?;
            identifier(&service, "service")?;
            let run_services = raw
                .run_services
                .iter()
                .map(|service| substitute(service, &variables))
                .collect::<Result<Vec<_>>>()?;
            for service in &run_services {
                identifier(service, "runServices")?;
            }
            if !run_services.is_empty() && !run_services.contains(&service) {
                return Err(invalid("runServices must include service"));
            }
            Source::Compose {
                files,
                service,
                run_services,
            }
        } else {
            let build = raw.build.unwrap_or(RawBuild {
                dockerfile: raw.dockerfile.unwrap_or_default(),
                context: raw.context,
                args: BTreeMap::new(),
                target: None,
            });
            let dockerfile = scoped_path(
                &workspace,
                base,
                Path::new(&substitute(&build.dockerfile, &variables)?),
                false,
            )?;
            let context = scoped_path(
                &workspace,
                base,
                Path::new(&substitute(
                    build.context.as_deref().unwrap_or("."),
                    &variables,
                )?),
                true,
            )?;
            let target = build
                .target
                .map(|target| substitute(&target, &variables))
                .transpose()?;
            if let Some(target) = &target {
                identifier(target, "build.target")?;
            }
            Source::Dockerfile {
                dockerfile,
                context,
                args: environment(&build.args, &variables)?,
                target,
            }
        };
        let remote_user = raw
            .remote_user
            .map(|user| substitute(&user, &variables))
            .transpose()?;
        let container_user = raw
            .container_user
            .map(|user| substitute(&user, &variables))
            .transpose()?;
        for user in [&remote_user, &container_user].into_iter().flatten() {
            user_name(user)?;
        }
        let container_env = environment(&raw.container_env, &variables)?;
        for (key, value) in &raw.remote_env {
            env_key(key)?;
            // Validate deferred containerEnv references without guessing values.
            substitute(
                value,
                &Variables {
                    container_env: Some(&BTreeMap::new()),
                    ..variables
                },
            )?;
        }
        let run_args = safe_run_args(&raw.run_args, &variables)?;
        let mut lifecycle = BTreeMap::new();
        for (stage, command) in [
            (Lifecycle::OnCreate, raw.on_create_command),
            (Lifecycle::UpdateContent, raw.update_content_command),
            (Lifecycle::PostCreate, raw.post_create_command),
            (Lifecycle::PostStart, raw.post_start_command),
            (Lifecycle::PostAttach, raw.post_attach_command),
        ] {
            if let Some(command) = command {
                let command = if let Some(script) = command.as_str() {
                    no_nul(script)?;
                    if script.contains("${") {
                        return Err(unsupported(
                            "variable substitution in lifecycle shell strings; use an argv array",
                        ));
                    }
                    LifecycleCommand::Shell(script.to_owned())
                } else if command.is_object() {
                    return Err(unsupported("parallel lifecycle command objects"));
                } else {
                    let argv = strings(&command, "lifecycle command")?;
                    let argv = argv
                        .iter()
                        .map(|arg| substitute(arg, &variables))
                        .collect::<Result<Vec<_>>>()?;
                    command_argv(&argv)?;
                    LifecycleCommand::Argv(argv)
                };
                lifecycle.insert(stage, command);
            }
        }
        let shutdown = match raw.shutdown_action.as_deref() {
            Some("none") => ShutdownAction::None,
            Some("stopContainer") if !compose => ShutdownAction::StopContainer,
            Some("stopCompose") if compose => ShutdownAction::StopCompose,
            None if compose => ShutdownAction::StopCompose,
            None => ShutdownAction::StopContainer,
            _ => {
                return Err(invalid(
                    "shutdownAction does not match the container source",
                ));
            }
        };
        let name = raw
            .name
            .map(|name| substitute(&name, &variables))
            .transpose()?;
        Ok(Self {
            file,
            workspace,
            name,
            source,
            mount,
            folder,
            container_env,
            remote_env: raw.remote_env,
            local_env,
            remote_user,
            container_user,
            run_args,
            lifecycle,
            shutdown,
            override_command: raw.override_command.unwrap_or(!compose),
            init: raw.init,
        })
    }

    pub fn source(&self) -> &Source {
        &self.source
    }
    pub fn file(&self) -> &Path {
        &self.file
    }
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    pub fn working_folder(&self) -> &str {
        &self.folder
    }
    pub fn workspace_mount(&self) -> Option<&WorkspaceMount> {
        self.mount.as_ref()
    }
    pub fn container_env(&self) -> &BTreeMap<String, String> {
        &self.container_env
    }
    pub fn lifecycle(&self) -> &BTreeMap<Lifecycle, LifecycleCommand> {
        &self.lifecycle
    }
    pub fn shutdown_action(&self) -> ShutdownAction {
        self.shutdown
    }

    pub fn limitations(&self) -> &'static [&'static str] {
        &[
            "No Features, extra mounts, port forwarding, customizations, image metadata or host lifecycle commands",
            "No UID/GID rewriting or user shell environment probing, including their specification defaults",
            "Only project-local existing host paths; no Docker socket, external volumes or external networks",
            "Lifecycle stages are synchronous; parent schedules creation, content updates, starts and attachments",
            "Compose requires config --format json; service scaling, profiles and runtime overrides are unsupported",
        ]
    }
}

struct Variables<'a> {
    workspace: &'a Path,
    container_folder: &'a str,
    local_env: &'a BTreeMap<String, String>,
    container_env: Option<&'a BTreeMap<String, String>>,
}

fn substitute(input: &str, variables: &Variables<'_>) -> Result<String> {
    no_nul(input)?;
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        if output.len() + start > MAX_CONFIG {
            return Err(invalid("expanded value exceeds 1 MiB"));
        }
        output.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let end = tail
            .find('}')
            .ok_or_else(|| invalid("unterminated variable"))?;
        let variable = &tail[..end];
        let value = match variable {
            "localWorkspaceFolder" => path_text(variables.workspace)?.to_owned(),
            "localWorkspaceFolderBasename" => variables
                .workspace
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| invalid("workspace name is not UTF-8"))?
                .to_owned(),
            "containerWorkspaceFolder" => variables.container_folder.to_owned(),
            "containerWorkspaceFolderBasename" => variables
                .container_folder
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_owned(),
            _ => {
                let (kind, name) = variable
                    .split_once(':')
                    .ok_or_else(|| unsupported(format!("variable {variable}")))?;
                let (name, default) = name.split_once(':').unwrap_or((name, ""));
                env_key(name)?;
                let env = match kind {
                    "localEnv" => variables.local_env,
                    "containerEnv" => variables.container_env.ok_or_else(|| {
                        unsupported("containerEnv variables are supported only in remoteEnv")
                    })?,
                    _ => return Err(unsupported(format!("variable {variable}"))),
                };
                env.get(name).map_or(default, String::as_str).to_owned()
            }
        };
        no_nul(&value)?;
        if output.len() + value.len() > MAX_CONFIG {
            return Err(invalid("expanded value exceeds 1 MiB"));
        }
        output.push_str(&value);
        rest = &tail[end + 1..];
    }
    if output.len() + rest.len() > MAX_CONFIG {
        return Err(invalid("expanded value exceeds 1 MiB"));
    }
    output.push_str(rest);
    Ok(output)
}

fn environment(
    input: &BTreeMap<String, String>,
    variables: &Variables<'_>,
) -> Result<BTreeMap<String, String>> {
    input
        .iter()
        .map(|(key, value)| {
            env_key(key)?;
            Ok((key.clone(), substitute(value, variables)?))
        })
        .collect()
}

fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))
}

fn scoped_path(root: &Path, base: &Path, path: &Path, directory: bool) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(invalid("empty host path"));
    }
    let path = canonical(&base.join(path))?;
    if !path.starts_with(root) {
        return Err(invalid(format!(
            "host path is outside the workspace: {}",
            path.display()
        )));
    }
    if (directory && !path.is_dir()) || (!directory && !path.is_file()) {
        return Err(invalid(format!("wrong host path type: {}", path.display())));
    }
    Ok(path)
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| unsupported("non-UTF-8 configuration paths"))
}

fn no_nul(value: &str) -> Result<()> {
    if value.contains('\0') {
        Err(invalid("NUL in value"))
    } else {
        Ok(())
    }
}

fn container_path(path: &str) -> Result<()> {
    no_nul(path)?;
    if !path.starts_with('/')
        || path.contains(['\\', '\n', '\r'])
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(invalid(
            "container path must be absolute without dot components",
        ));
    }
    Ok(())
}

fn container_contains(parent: &str, child: &str) -> bool {
    let parent = parent.trim_end_matches('/');
    child == parent
        || child
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn env_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.starts_with(|c: char| c.is_ascii_digit())
        || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        Err(invalid(format!("invalid environment name: {key}")))
    } else {
        Ok(())
    }
}

fn identifier(value: &str, field: &str) -> Result<()> {
    if value.is_empty()
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    {
        Err(invalid(format!("invalid {field}")))
    } else {
        Ok(())
    }
}

fn user_name(user: &str) -> Result<()> {
    if user.is_empty()
        || user.split(':').count() > 2
        || user
            .split(':')
            .any(|part| identifier(part, "user").is_err())
    {
        Err(invalid("invalid container user"))
    } else {
        Ok(())
    }
}

fn image_name(image: &str) -> Result<()> {
    if image.is_empty()
        || !image.as_bytes()[0].is_ascii_alphanumeric()
        || !image
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/_.-:@".contains(&c))
    {
        Err(invalid("invalid image reference"))
    } else {
        Ok(())
    }
}

fn command_argv(argv: &[String]) -> Result<()> {
    if argv.is_empty() || argv[0].is_empty() || argv[0].starts_with('-') {
        return Err(invalid("command requires an executable"));
    }
    for argument in argv {
        no_nul(argument)?;
    }
    Ok(())
}

fn strings(value: &Value, field: &str) -> Result<Vec<String>> {
    if let Some(value) = value.as_str() {
        return Ok(vec![value.to_owned()]);
    }
    value
        .as_array()
        .ok_or_else(|| invalid(format!("{field} must be a string or array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid(format!("{field} requires string values")))
        })
        .collect()
}

fn parse_mount(value: &str, root: &Path, base: &Path) -> Result<WorkspaceMount> {
    let mut fields = BTreeMap::new();
    for field in value.split(',') {
        let (key, value) = field.split_once('=').unwrap_or((field, ""));
        let key = match key {
            "src" => "source",
            "dst" | "destination" => "target",
            "ro" => "readonly",
            key => key,
        };
        if !["type", "source", "target", "readonly", "consistency"].contains(&key)
            || fields.insert(key, value).is_some()
        {
            return Err(unsupported("workspaceMount option or duplicate field"));
        }
    }
    if fields.get("type") != Some(&"bind") {
        return Err(unsupported("workspaceMount types other than bind"));
    }
    let source = fields
        .get("source")
        .ok_or_else(|| invalid("workspaceMount needs source"))?;
    let source = scoped_path(root, base, Path::new(source), true)?;
    mount_source(&source)?;
    let target = fields
        .get("target")
        .ok_or_else(|| invalid("workspaceMount needs target"))?
        .to_string();
    container_path(&target)?;
    if target == "/" || target.contains(['"', ',']) {
        return Err(invalid("invalid workspace mount target"));
    }
    let readonly = match fields.get("readonly") {
        None | Some(&"false") => false,
        Some(&"" | &"true") => true,
        _ => return Err(invalid("invalid readonly value")),
    };
    let consistency = fields.get("consistency").map(|value| value.to_string());
    if consistency
        .as_deref()
        .is_some_and(|value| !["cached", "delegated", "consistent"].contains(&value))
    {
        return Err(invalid("invalid mount consistency"));
    }
    Ok(WorkspaceMount {
        source,
        target,
        readonly,
        consistency,
    })
}

fn mount_source(path: &Path) -> Result<()> {
    if path_text(path)?.contains([',', '"', '\n', '\r']) {
        return Err(unsupported(
            "mount paths containing commas, quotes or line breaks",
        ));
    }
    Ok(())
}

fn safe_run_args(args: &[String], variables: &Variables<'_>) -> Result<Vec<String>> {
    let mut output = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let argument = substitute(&args[i], variables)?;
        if argument == "--init" {
            output.push(argument);
            i += 1;
            continue;
        }
        let (flag, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(flag, value)| {
                (flag, Some(value))
            });
        if !["--cpus", "--memory", "--shm-size", "--hostname"].contains(&flag) {
            return Err(unsupported(format!(
                "runArgs flag {flag}; mounts, daemon controls, privileges and command overrides are not allowed"
            )));
        }
        let value = if let Some(value) = inline {
            value.to_owned()
        } else {
            i += 1;
            substitute(
                args.get(i)
                    .ok_or_else(|| invalid("runArgs flag needs a value"))?,
                variables,
            )?
        };
        let valid = match flag {
            "--cpus" => value
                .parse::<f64>()
                .is_ok_and(|value| value.is_finite() && value > 0.0),
            "--hostname" => identifier(&value, "hostname").is_ok(),
            _ => {
                let digits = value.bytes().take_while(u8::is_ascii_digit).count();
                digits > 0
                    && value[..digits].parse::<u64>().is_ok_and(|size| size > 0)
                    && ["", "b", "k", "kb", "m", "mb", "g", "gb"]
                        .contains(&value[digits..].to_ascii_lowercase().as_str())
            }
        };
        if !valid {
            return Err(invalid(format!("invalid runArgs value for {flag}")));
        }
        output.push(format!("{flag}={value}"));
        i += 1;
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    ComposeConfig,
    Build,
    Create,
    ResolveId,
    Start,
    Inspect,
    Exec,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandPlan {
    pub operation: Operation,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// Compose uses a validated, frozen JSON model instead of reopening YAML.
    pub stdin: Option<Vec<u8>>,
    /// The runner must remove these variables before applying `env`.
    pub remove_env: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Default)]
pub struct CommandOutput {
    /// None means termination by a signal; success is exactly Some(0).
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// The parent runner owns spawning, cancellation, timeouts and output limits.
/// It must pass argv directly, never format it into a host shell command.
pub trait Runner {
    fn run(&mut self, plan: &CommandPlan) -> io::Result<CommandOutput>;
}

impl<F: FnMut(&CommandPlan) -> io::Result<CommandOutput>> Runner for F {
    fn run(&mut self, plan: &CommandPlan) -> io::Result<CommandOutput> {
        self(plan)
    }
}

fn execute(runner: &mut impl Runner, plan: &CommandPlan) -> Result<Vec<u8>> {
    let output = runner
        .run(plan)
        .map_err(|error| Error::Io(error.to_string()))?;
    if output.stdout.len() > MAX_OUTPUT || output.stderr.len() > MAX_OUTPUT {
        return Err(invalid("Docker output exceeds 8 MiB"));
    }
    if output.code != Some(0) {
        return Err(Error::Command {
            operation: plan.operation,
            code: output.code,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerId(String);

impl ContainerId {
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        if value.len() != 64 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid("Docker did not return one full container ID"));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct Container {
    id: ContainerId,
    folder: String,
    user: Option<String>,
    env: BTreeMap<String, String>,
    running: bool,
}

impl Container {
    pub fn id(&self) -> &ContainerId {
        &self.id
    }
    pub fn working_folder(&self) -> &str {
        &self.folder
    }
    pub fn remote_user(&self) -> Option<&str> {
        self.user.as_deref()
    }
    /// Overrides for exec; other variables are inherited from the container.
    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }
    pub fn running(&self) -> bool {
        self.running
    }
}

pub struct Runtime {
    config: Config,
    docker: PathBuf,
    instance: String,
    compose: Option<Vec<u8>>,
    compose_services: Vec<String>,
}

impl Runtime {
    /// `instance` is a stable, parent-owned identifier, not a display name.
    /// The parent supplies an optional executable path; absence means `docker`.
    pub fn new(config: Config, docker: Option<PathBuf>, instance: &str) -> Result<Self> {
        if instance.is_empty()
            || !instance.as_bytes()[0].is_ascii_lowercase()
            || !instance
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
        {
            return Err(invalid(
                "instance must be a lowercase Docker Compose project identifier",
            ));
        }
        let docker = docker.unwrap_or_else(|| PathBuf::from("docker"));
        if docker.as_os_str().is_empty() {
            return Err(invalid("empty Docker program"));
        }
        Ok(Self {
            config,
            docker,
            instance: instance.to_owned(),
            compose: None,
            compose_services: Vec::new(),
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    fn plan(&self, operation: Operation, args: Vec<OsString>) -> CommandPlan {
        CommandPlan {
            operation,
            program: self.docker.clone(),
            args,
            cwd: self.config.workspace.clone(),
            stdin: None,
            remove_env: Vec::new(),
            env: BTreeMap::new(),
        }
    }

    fn compose_plan(
        &self,
        operation: Operation,
        command: &[&str],
        frozen: bool,
    ) -> Result<CommandPlan> {
        let Source::Compose { files, .. } = &self.config.source else {
            return Err(invalid("not a Compose configuration"));
        };
        let mut args = vec![
            "compose".into(),
            "--ansi=never".into(),
            "--project-name".into(),
            self.instance.clone().into(),
            "--project-directory".into(),
            files[0]
                .parent()
                .ok_or_else(|| invalid("Compose file has no parent"))?
                .as_os_str()
                .to_owned(),
        ];
        if frozen {
            args.extend(["--file".into(), "-".into()]);
        } else {
            for file in files {
                args.extend(["--file".into(), file.as_os_str().to_owned()]);
            }
        }
        args.extend(command.iter().map(OsString::from));
        let mut plan = self.plan(operation, args);
        // Ambient COMPOSE_* settings must not select extra files or services.
        plan.remove_env = [
            "COMPOSE_FILE",
            "COMPOSE_PROJECT_NAME",
            "COMPOSE_PROFILES",
            "COMPOSE_ENV_FILES",
            "COMPOSE_PATH_SEPARATOR",
            "COMPOSE_CONVERT_WINDOWS_PATHS",
            "COMPOSE_REMOVE_ORPHANS",
            "COMPOSE_IGNORE_ORPHANS",
            "COMPOSE_COMPATIBILITY",
            "COMPOSE_STATUS_STDOUT",
            "COMPOSE_MENU",
            "COMPOSE_EXPERIMENTAL",
            "COMPOSE_BAKE",
        ]
        .iter()
        .map(|name| name.to_string())
        .collect();
        // Substitution inputs must not become Docker daemon or Compose control
        // settings. Transport selection is the parent runner's responsibility.
        plan.env = self
            .config
            .local_env
            .iter()
            .filter(|(key, _)| !key.starts_with("DOCKER_") && !key.starts_with("COMPOSE_"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        plan.env
            .insert("COMPOSE_DISABLE_ENV_FILE".into(), "1".into());
        plan.env.insert("COMPOSE_REMOVE_ORPHANS".into(), "0".into());
        plan.env.insert("COMPOSE_STATUS_STDOUT".into(), "0".into());
        // Match the devcontainer project-root .env convention explicitly.
        if !frozen {
            let dotenv = self.config.workspace.join(".env");
            if dotenv.exists() {
                let dotenv = scoped_path(
                    &self.config.workspace,
                    &self.config.workspace,
                    &dotenv,
                    false,
                )?;
                let index = plan
                    .args
                    .iter()
                    .position(|arg| arg == "config")
                    .ok_or_else(|| invalid("missing Compose config command"))?;
                plan.args.splice(
                    index..index,
                    [OsString::from("--env-file"), dotenv.into_os_string()],
                );
            }
        }
        if frozen {
            plan.stdin = Some(self.compose.clone().ok_or_else(|| {
                invalid("resolve and validate Compose configuration before planning operations")
            })?);
        }
        Ok(plan)
    }

    pub fn compose_config_plan(&self) -> Result<Option<CommandPlan>> {
        if matches!(self.config.source, Source::Compose { .. }) {
            self.compose_plan(
                Operation::ComposeConfig,
                &["config", "--format", "json"],
                false,
            )
            .map(Some)
        } else {
            Ok(None)
        }
    }

    /// Read-only Docker CLI operation, still subject to parent configuration
    /// consent: Compose can read env_file/include references while resolving.
    pub fn resolve_compose(&mut self, runner: &mut impl Runner) -> Result<()> {
        if let Some(plan) = self.compose_config_plan()? {
            self.compose = None;
            let output = execute(runner, &plan)?;
            self.accept_compose_json(&output)?;
        }
        Ok(())
    }

    pub fn accept_compose_json(&mut self, json: &[u8]) -> Result<()> {
        self.compose = None;
        self.compose_services.clear();
        if json.len() > MAX_OUTPUT {
            return Err(invalid("Compose model exceeds 8 MiB"));
        }
        let mut model: Value =
            serde_json::from_slice(json).map_err(|error| invalid(error.to_string()))?;
        validate_compose(&mut model, &self.config, &self.instance)?;
        if let Source::Compose { run_services, .. } = &self.config.source
            && !run_services.is_empty()
        {
            self.compose_services = run_services.clone();
            let mut index = 0;
            while index < self.compose_services.len() {
                if let Some(dependencies) =
                    model["services"][&self.compose_services[index]]["depends_on"].as_object()
                {
                    for dependency in dependencies.keys() {
                        if !self.compose_services.contains(dependency) {
                            self.compose_services.push(dependency.clone());
                        }
                    }
                }
                index += 1;
            }
        }
        // Compose would interpolate an already-resolved dollar a second time
        // when reading stdin. Freeze literals with Compose's $$ escape.
        escape_dollars(&mut model);
        self.compose =
            Some(serde_json::to_vec(&model).map_err(|error| invalid(error.to_string()))?);
        Ok(())
    }

    pub fn build_plan(&self) -> Result<Option<CommandPlan>> {
        match &self.config.source {
            Source::Image(_) => Ok(None),
            Source::Compose { .. } => {
                let mut plan = self.compose_plan(Operation::Build, &["build"], true)?;
                plan.args
                    .extend(self.compose_services.iter().map(OsString::from));
                Ok(Some(plan))
            }
            Source::Dockerfile {
                dockerfile,
                context,
                args,
                target,
            } => {
                let mut argv = vec![
                    "build".into(),
                    "--tag".into(),
                    self.image_tag().into(),
                    "--file".into(),
                    dockerfile.as_os_str().to_owned(),
                ];
                for (key, value) in args {
                    argv.extend(["--build-arg".into(), format!("{key}={value}").into()]);
                }
                if let Some(target) = target {
                    argv.extend(["--target".into(), target.into()]);
                }
                argv.push(context.as_os_str().to_owned());
                Ok(Some(self.plan(Operation::Build, argv)))
            }
        }
    }

    fn image_tag(&self) -> String {
        format!("solder-devcontainer/{}:local", self.instance)
    }

    pub fn create_plan(&self) -> Result<CommandPlan> {
        if matches!(self.config.source, Source::Compose { .. }) {
            let mut plan = self.compose_plan(
                Operation::Create,
                &[
                    "create",
                    "--no-build",
                    "--pull",
                    "never",
                    "--force-recreate",
                ],
                true,
            )?;
            plan.args
                .extend(self.compose_services.iter().map(OsString::from));
            return Ok(plan);
        }
        let mut args = vec![
            "create".into(),
            "--pull=never".into(),
            "--name".into(),
            self.instance.clone().into(),
        ];
        if self.config.init {
            args.push("--init".into());
        }
        let mount = self
            .config
            .mount
            .as_ref()
            .ok_or_else(|| invalid("missing workspace mount"))?;
        mount_source(&mount.source)?;
        let mut value = format!(
            "type=bind,source={},target={}",
            path_text(&mount.source)?,
            mount.target
        );
        if mount.readonly {
            value.push_str(",readonly");
        }
        if let Some(consistency) = &mount.consistency {
            value.push_str(&format!(",consistency={consistency}"));
        }
        args.extend([
            "--mount".into(),
            value.into(),
            "--workdir".into(),
            self.config.folder.clone().into(),
        ]);
        for (key, value) in &self.config.container_env {
            args.extend(["--env".into(), format!("{key}={value}").into()]);
        }
        if let Some(user) = &self.config.container_user {
            args.extend(["--user".into(), user.into()]);
        }
        args.extend(self.config.run_args.iter().map(OsString::from));
        if self.config.override_command {
            args.extend(["--entrypoint".into(), "/bin/sh".into()]);
        }
        args.push(match &self.config.source {
            Source::Image(image) => image.clone().into(),
            _ => self.image_tag().into(),
        });
        if self.config.override_command {
            args.extend(["-c".into(), "while sleep 1000; do :; done".into()]);
        }
        Ok(self.plan(Operation::Create, args))
    }

    pub fn resolve_id_plan(&self) -> Result<CommandPlan> {
        let Source::Compose { service, .. } = &self.config.source else {
            return Err(invalid("native create returns its container ID directly"));
        };
        let mut plan =
            self.compose_plan(Operation::ResolveId, &["ps", "--all", "--quiet"], true)?;
        plan.args.push(service.into());
        Ok(plan)
    }

    pub fn start_plan(&self, id: &ContainerId) -> Result<CommandPlan> {
        if matches!(self.config.source, Source::Compose { .. }) {
            let mut plan = self.compose_plan(Operation::Start, &["start"], true)?;
            plan.args
                .extend(self.compose_services.iter().map(OsString::from));
            Ok(plan)
        } else {
            Ok(self.plan(Operation::Start, vec!["start".into(), id.as_str().into()]))
        }
    }

    pub fn inspect_plan(&self, id: &ContainerId) -> CommandPlan {
        self.plan(
            Operation::Inspect,
            vec!["container".into(), "inspect".into(), id.as_str().into()],
        )
    }

    /// Explicit stop; the parent decides whether shutdownAction calls it.
    pub fn stop_plan(&self, id: &ContainerId) -> Result<CommandPlan> {
        if matches!(self.config.source, Source::Compose { .. }) {
            let mut plan = self.compose_plan(Operation::Stop, &["stop"], true)?;
            plan.args
                .extend(self.compose_services.iter().map(OsString::from));
            Ok(plan)
        } else {
            Ok(self.plan(Operation::Stop, vec!["stop".into(), id.as_str().into()]))
        }
    }

    pub fn exec_plan(
        &self,
        container: &Container,
        argv: &[String],
        interactive: bool,
        tty: bool,
    ) -> Result<CommandPlan> {
        command_argv(argv)?;
        if !container.running() {
            return Err(invalid("container is not running"));
        }
        let mut args = vec!["exec".into()];
        if interactive {
            args.push("--interactive".into());
        }
        if tty {
            args.push("--tty".into());
        }
        args.extend(["--workdir".into(), container.working_folder().into()]);
        if let Some(user) = &container.user {
            args.extend(["--user".into(), user.into()]);
        }
        for (key, value) in &container.env {
            args.extend(["--env".into(), format!("{key}={value}").into()]);
        }
        args.push(container.id().as_str().into());
        args.extend(argv.iter().map(OsString::from));
        Ok(self.plan(Operation::Exec, args))
    }

    pub fn lifecycle_plan(
        &self,
        container: &Container,
        stage: Lifecycle,
    ) -> Result<Option<CommandPlan>> {
        let Some(command) = self.config.lifecycle.get(&stage) else {
            return Ok(None);
        };
        let argv = match command {
            LifecycleCommand::Argv(argv) => argv.clone(),
            LifecycleCommand::Shell(script) => {
                vec!["/bin/sh".to_owned(), "-c".to_owned(), script.clone()]
            }
        };
        self.exec_plan(container, &argv, false, false).map(Some)
    }

    pub fn build(&self, runner: &mut impl Runner) -> Result<()> {
        if let Some(plan) = self.build_plan()? {
            execute(runner, &plan)?;
        }
        Ok(())
    }

    pub fn create(&self, runner: &mut impl Runner) -> Result<ContainerId> {
        let mut output = execute(runner, &self.create_plan()?)?;
        if matches!(self.config.source, Source::Compose { .. }) {
            output = execute(runner, &self.resolve_id_plan()?)?;
        }
        ContainerId::parse(
            std::str::from_utf8(&output).map_err(|_| invalid("container ID is not UTF-8"))?,
        )
    }

    pub fn inspect(&self, id: &ContainerId, runner: &mut impl Runner) -> Result<Container> {
        self.container_from_inspect(id, &execute(runner, &self.inspect_plan(id))?)
    }

    pub fn start(&self, id: &ContainerId, runner: &mut impl Runner) -> Result<Container> {
        execute(runner, &self.start_plan(id)?)?;
        let container = self.inspect(id, runner)?;
        if !container.running() {
            return Err(invalid("container did not stay running"));
        }
        Ok(container)
    }

    pub fn stop(&self, id: &ContainerId, runner: &mut impl Runner) -> Result<()> {
        execute(runner, &self.stop_plan(id)?)?;
        Ok(())
    }

    /// Stops on the first failed stage; the parent must not schedule subsequent
    /// stages after an error, and persists which creation stages already ran.
    pub fn run_lifecycle(
        &self,
        container: &Container,
        stages: &[Lifecycle],
        runner: &mut impl Runner,
    ) -> Result<()> {
        for stage in stages {
            if let Some(plan) = self.lifecycle_plan(container, *stage)? {
                execute(runner, &plan)?;
            }
        }
        Ok(())
    }

    pub fn container_from_inspect(&self, id: &ContainerId, json: &[u8]) -> Result<Container> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Inspect {
            id: String,
            state: State,
            config: InspectConfig,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct State {
            running: bool,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct InspectConfig {
            #[serde(default)]
            env: Option<Vec<String>>,
            #[serde(default)]
            user: String,
            #[serde(default)]
            labels: Option<BTreeMap<String, String>>,
        }
        if json.len() > MAX_OUTPUT {
            return Err(invalid("inspect output exceeds 8 MiB"));
        }
        let mut entries: Vec<Inspect> =
            serde_json::from_slice(json).map_err(|error| invalid(error.to_string()))?;
        if entries.len() != 1 {
            return Err(invalid("inspect must return exactly one container"));
        }
        let entry = entries.remove(0);
        if ContainerId::parse(&entry.id)? != *id {
            return Err(invalid("inspect returned a different container ID"));
        }
        if entry
            .config
            .labels
            .as_ref()
            .is_some_and(|labels| labels.contains_key("devcontainer.metadata"))
        {
            return Err(unsupported(
                "devcontainer.metadata image labels require metadata merging",
            ));
        }
        let mut container_env = BTreeMap::new();
        for pair in entry.config.env.unwrap_or_default() {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| invalid("invalid inspect environment"))?;
            env_key(key)?;
            no_nul(value)?;
            container_env.insert(key.to_owned(), value.to_owned());
        }
        let variables = Variables {
            workspace: &self.config.workspace,
            container_folder: self
                .config
                .mount
                .as_ref()
                .map_or(self.config.folder.as_str(), |mount| mount.target.as_str()),
            local_env: &self.config.local_env,
            container_env: Some(&container_env),
        };
        let env = environment(&self.config.remote_env, &variables)?;
        let user = self
            .config
            .remote_user
            .clone()
            .or_else(|| (!entry.config.user.is_empty()).then_some(entry.config.user));
        if let Some(user) = &user {
            user_name(user)?;
        }
        Ok(Container {
            id: id.clone(),
            folder: self.config.folder.clone(),
            user,
            env,
            running: entry.state.running,
        })
    }
}

fn allowed_keys(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
    context: &str,
) -> Result<()> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(unsupported(format!("{context}.{key}")));
        }
    }
    Ok(())
}

fn json_object<'a>(value: &'a Value, context: &str) -> Result<&'a serde_json::Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{context} must be an object")))
}

fn json_string<'a>(value: &'a Value, context: &str) -> Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| invalid(format!("{context} must be a string")))
}

fn json_bool(value: &Value, context: &str) -> Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| invalid(format!("{context} must be a boolean")))
}

fn validate_literals(value: &Value) -> Result<()> {
    match value {
        Value::String(value) => no_nul(value),
        Value::Array(values) => {
            for value in values {
                validate_literals(value)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for (key, value) in values {
                no_nul(key)?;
                validate_literals(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn compose_environment(value: &Value) -> Result<()> {
    for (key, value) in json_object(value, "Compose environment")? {
        env_key(key)?;
        json_string(value, "Compose environment value")?;
    }
    Ok(())
}

fn validate_compose(model: &mut Value, config: &Config, instance: &str) -> Result<()> {
    validate_literals(model)?;
    let Source::Compose {
        service,
        run_services,
        ..
    } = &config.source
    else {
        return Err(invalid("not a Compose configuration"));
    };
    let root = model
        .as_object_mut()
        .ok_or_else(|| invalid("Compose model must be an object"))?;
    allowed_keys(
        root,
        &["name", "services", "networks", "volumes", "version"],
        "Compose",
    )?;
    if let Some(name) = root.get("name")
        && json_string(name, "Compose name")? != instance
    {
        return Err(invalid("Compose project name does not match instance"));
    }
    root.insert("name".into(), instance.into());
    let services = root
        .get_mut("services")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid("Compose requires services"))?;
    if !services.contains_key(service)
        || run_services
            .iter()
            .any(|service| !services.contains_key(service))
    {
        return Err(invalid("requested Compose service does not exist"));
    }
    let service_names: Vec<_> = services.keys().cloned().collect();
    // All services are checked, including dependencies not explicitly listed
    // by runServices: Compose may create them on behalf of a selected service.
    for (name, service) in services.iter_mut() {
        identifier(name, "Compose service")?;
        let object = service
            .as_object_mut()
            .ok_or_else(|| invalid("Compose service must be an object"))?;
        allowed_keys(
            object,
            &[
                "image",
                "build",
                "command",
                "entrypoint",
                "environment",
                "working_dir",
                "user",
                "volumes",
                "depends_on",
                "networks",
                "init",
                "tty",
                "stdin_open",
                "restart",
                "expose",
                "hostname",
                "healthcheck",
            ],
            "Compose service",
        )?;
        if !object.contains_key("image") && !object.contains_key("build") {
            return Err(invalid("Compose service requires image or build"));
        }
        if let Some(image) = object.get("image") {
            image_name(json_string(image, "Compose image")?)?;
        }
        if let Some(build) = object.get_mut("build") {
            let build = build
                .as_object_mut()
                .ok_or_else(|| invalid("Compose build must be normalized to an object"))?;
            allowed_keys(
                build,
                &["context", "dockerfile", "args", "target"],
                "Compose build",
            )?;
            let context = build
                .get("context")
                .ok_or_else(|| invalid("Compose build requires context"))?;
            let context = scoped_path(
                &config.workspace,
                &config.workspace,
                Path::new(json_string(context, "Compose build context")?),
                true,
            )?;
            let dockerfile = build
                .get("dockerfile")
                .map(|value| json_string(value, "Compose dockerfile"))
                .transpose()?
                .unwrap_or("Dockerfile");
            let dockerfile =
                scoped_path(&config.workspace, &context, Path::new(dockerfile), false)?;
            if let Some(target) = build.get("target") {
                identifier(
                    json_string(target, "Compose build target")?,
                    "Compose build target",
                )?;
            }
            if let Some(args) = build.get("args") {
                compose_environment(args)?;
            }
            build.insert("context".into(), path_text(&context)?.into());
            build.insert("dockerfile".into(), path_text(&dockerfile)?.into());
        }
        for key in ["command", "entrypoint"] {
            if let Some(value) = object.get(key)
                && !value.is_null()
            {
                strings(value, key)?;
            }
        }
        if let Some(env) = object.get("environment") {
            compose_environment(env)?;
        }
        if let Some(folder) = object.get("working_dir") {
            container_path(json_string(folder, "Compose working_dir")?)?;
        }
        if let Some(user) = object.get("user") {
            user_name(json_string(user, "Compose user")?)?;
        }
        if let Some(hostname) = object.get("hostname") {
            identifier(
                json_string(hostname, "Compose hostname")?,
                "Compose hostname",
            )?;
        }
        for key in ["init", "tty", "stdin_open"] {
            if let Some(value) = object.get(key) {
                json_bool(value, key)?;
            }
        }
        if let Some(restart) = object.get("restart")
            && !["no", "always", "on-failure", "unless-stopped"]
                .contains(&json_string(restart, "Compose restart")?)
        {
            return Err(unsupported("Compose restart policy"));
        }
        if let Some(expose) = object.get("expose") {
            for port in expose
                .as_array()
                .ok_or_else(|| invalid("Compose expose must be an array"))?
            {
                if let Some(port) = port.as_u64() {
                    if !(1..=65535).contains(&port) {
                        return Err(invalid("invalid exposed port"));
                    }
                } else {
                    let port = json_string(port, "Compose expose port")?;
                    let (number, protocol) = port.split_once('/').unwrap_or((port, "tcp"));
                    if !["tcp", "udp"].contains(&protocol)
                        || number.parse::<u16>().map_or(true, |number| number == 0)
                    {
                        return Err(unsupported("Compose expose ranges or invalid ports"));
                    }
                }
            }
        }
        if let Some(dependencies) = object.get("depends_on") {
            for (dependency, options) in json_object(dependencies, "Compose depends_on")? {
                if !service_names.contains(dependency) {
                    return Err(invalid("unknown Compose dependency"));
                }
                let options = json_object(options, "Compose dependency")?;
                allowed_keys(
                    options,
                    &["condition", "required", "restart"],
                    "Compose dependency",
                )?;
                if let Some(condition) = options.get("condition")
                    && ![
                        "service_started",
                        "service_healthy",
                        "service_completed_successfully",
                    ]
                    .contains(&json_string(condition, "dependency condition")?)
                {
                    return Err(invalid("invalid Compose dependency condition"));
                }
                for key in ["required", "restart"] {
                    if let Some(value) = options.get(key) {
                        json_bool(value, key)?;
                    }
                }
            }
        }
        if let Some(healthcheck) = object.get("healthcheck") {
            let healthcheck = json_object(healthcheck, "Compose healthcheck")?;
            allowed_keys(
                healthcheck,
                &[
                    "test",
                    "interval",
                    "timeout",
                    "start_period",
                    "start_interval",
                    "retries",
                    "disable",
                ],
                "Compose healthcheck",
            )?;
            for (key, value) in healthcheck {
                match key.as_str() {
                    "test" => {
                        strings(value, "healthcheck test")?;
                    }
                    "disable" => {
                        json_bool(value, "healthcheck disable")?;
                    }
                    "retries" => {
                        if value.as_u64().is_none() {
                            return Err(invalid("healthcheck retries must be positive"));
                        }
                    }
                    _ => {
                        json_string(value, "healthcheck duration")?;
                    }
                }
            }
        }
        if let Some(networks) = object.get("networks") {
            for (name, options) in json_object(networks, "Compose service networks")? {
                identifier(name, "Compose network")?;
                if !options.is_null() {
                    let options = json_object(options, "Compose network options")?;
                    allowed_keys(options, &["aliases"], "Compose network options")?;
                    if let Some(aliases) = options.get("aliases") {
                        for alias in strings(aliases, "network aliases")? {
                            identifier(&alias, "network alias")?;
                        }
                    }
                }
            }
        }
        if let Some(volumes) = object.get_mut("volumes") {
            for volume in volumes
                .as_array_mut()
                .ok_or_else(|| invalid("Compose volumes must be an array"))?
            {
                let volume = volume
                    .as_object_mut()
                    .ok_or_else(|| invalid("Compose volumes must be normalized objects"))?;
                allowed_keys(
                    volume,
                    &[
                        "type",
                        "source",
                        "target",
                        "read_only",
                        "consistency",
                        "bind",
                        "volume",
                    ],
                    "Compose mount",
                )?;
                let target = volume
                    .get("target")
                    .ok_or_else(|| invalid("Compose mount requires target"))?;
                container_path(json_string(target, "Compose mount target")?)?;
                if let Some(readonly) = volume.get("read_only") {
                    json_bool(readonly, "Compose read_only")?;
                }
                if let Some(consistency) = volume.get("consistency")
                    && !["consistent", "cached", "delegated"]
                        .contains(&json_string(consistency, "Compose consistency")?)
                {
                    return Err(invalid("invalid Compose mount consistency"));
                }
                match volume.get("type").and_then(Value::as_str) {
                    Some("bind") => {
                        if volume.contains_key("volume") {
                            return Err(invalid("bind mount has volume options"));
                        }
                        let source = volume
                            .get("source")
                            .ok_or_else(|| invalid("Compose bind requires source"))?;
                        let source = Path::new(json_string(source, "Compose bind source")?);
                        let path = canonical(&config.workspace.join(source))?;
                        let path = scoped_path(
                            &config.workspace,
                            &config.workspace,
                            &path,
                            path.is_dir(),
                        )?;
                        if let Some(bind) = volume.get("bind") {
                            let bind = json_object(bind, "Compose bind options")?;
                            allowed_keys(
                                bind,
                                &["create_host_path", "propagation"],
                                "Compose bind",
                            )?;
                            if let Some(create) = bind.get("create_host_path") {
                                json_bool(create, "create_host_path")?;
                            }
                            if bind
                                .get("propagation")
                                .is_some_and(|value| value.as_str() != Some("rprivate"))
                            {
                                return Err(unsupported(
                                    "Compose bind propagation other than rprivate",
                                ));
                            }
                        }
                        volume.insert("source".into(), path_text(&path)?.into());
                        // It existed during validation. Never let Compose create
                        // a directory if the path disappears before execution.
                        volume.insert("bind".into(), serde_json::json!({"create_host_path": false, "propagation": "rprivate"}));
                    }
                    Some("volume") => {
                        if volume.contains_key("bind") {
                            return Err(invalid("volume mount has bind options"));
                        }
                        if let Some(source) = volume.get("source") {
                            identifier(
                                json_string(source, "Compose volume source")?,
                                "Compose volume",
                            )?;
                        }
                        if let Some(options) = volume.get("volume") {
                            let options = json_object(options, "Compose volume options")?;
                            allowed_keys(options, &["nocopy"], "Compose volume")?;
                            if let Some(nocopy) = options.get("nocopy") {
                                json_bool(nocopy, "nocopy")?;
                            }
                        }
                    }
                    _ => return Err(unsupported("Compose mount type")),
                }
            }
        }
    }
    for (kind, allowed) in [
        (
            "networks",
            &["name", "driver", "external", "internal", "attachable"][..],
        ),
        ("volumes", &["name", "driver", "external"][..]),
    ] {
        if let Some(resources) = root.get(kind) {
            for (name, resource) in json_object(resources, kind)? {
                identifier(name, kind)?;
                let resource = json_object(resource, kind)?;
                allowed_keys(resource, allowed, kind)?;
                if let Some(external) = resource.get("external")
                    && json_bool(external, "external")?
                {
                    return Err(unsupported("external Compose volumes or networks"));
                }
                if let Some(name) = resource.get("name") {
                    let name = json_string(name, "resource name")?;
                    identifier(name, "resource name")?;
                    if !name.starts_with(&format!("{instance}_")) {
                        return Err(unsupported("Compose resource names outside this instance"));
                    }
                }
                if let Some(driver) = resource.get("driver") {
                    let driver = json_string(driver, "Compose driver")?;
                    if driver
                        != if kind == "networks" {
                            "bridge"
                        } else {
                            "local"
                        }
                    {
                        return Err(unsupported("Compose resource driver"));
                    }
                }
                for key in ["internal", "attachable"] {
                    if let Some(value) = resource.get(key) {
                        json_bool(value, key)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn escape_dollars(value: &mut Value) {
    match value {
        Value::String(text) => *text = text.replace('$', "$$"),
        Value::Array(values) => {
            for value in values {
                escape_dollars(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                escape_dollars(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    struct Fixture {
        root: PathBuf,
        file: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "solder container tests {} {} $literal",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(root.join(".devcontainer")).unwrap();
            let root = root.canonicalize().unwrap();
            let file = root.join(".devcontainer/devcontainer.json");
            std::fs::write(&file, "{}").unwrap();
            std::fs::write(root.join(".devcontainer/Dockerfile"), "FROM scratch\n").unwrap();
            std::fs::write(root.join("compose base.yml"), "services: {}\n").unwrap();
            std::fs::write(root.join("compose override.yml"), "services: {}\n").unwrap();
            Self { root, file }
        }
        fn parse(&self, source: &str) -> Result<Config> {
            Config::parse(source, &self.file, &self.root, BTreeMap::new())
        }
        fn runtime(&self, source: &str) -> Runtime {
            Runtime::new(self.parse(source).unwrap(), None, "solder_test").unwrap()
        }
        fn compose(&self) -> Runtime {
            self.runtime(r#"{"dockerComposeFile":["../compose base.yml","../compose override.yml"],"service":"app","workspaceFolder":"/workspace","runServices":["app","db"]}"#)
        }
        fn model(&self) -> Value {
            serde_json::json!({
                "name": "solder_test",
                "services": {
                    "app": {"image":"alpine:3", "environment":{"LITERAL":"$HOME ${UNSET}"}, "volumes":[{"type":"bind", "source":self.root, "target":"/workspace", "bind":{"create_host_path":true}}], "depends_on":{"db":{"condition":"service_started","required":true}}, "networks":{"default":null}},
                    "db": {"image":"postgres:16", "volumes":[{"type":"volume","source":"data","target":"/var/lib/postgresql/data"}]}
                },
                "networks":{"default":{"name":"solder_test_default"}},
                "volumes":{"data":{"name":"solder_test_data"}}
            })
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn args(plan: &CommandPlan) -> Vec<String> {
        plan.args
            .iter()
            .map(|arg| arg.to_str().unwrap().to_owned())
            .collect()
    }
    fn success(output: impl Into<Vec<u8>>) -> io::Result<CommandOutput> {
        Ok(CommandOutput {
            code: Some(0),
            stdout: output.into(),
            stderr: Vec::new(),
        })
    }

    #[test]
    fn jsonc_image_paths_with_spaces_and_no_host_shell() {
        let fixture = Fixture::new();
        let runtime = fixture.runtime(
            r#"{
            // Shell characters in the project folder must remain one argument.
            "image":"alpine:3", "workspaceFolder":"${containerWorkspaceFolder}/src",
            "containerEnv":{"GREETING":"two words $HOME; still literal"},
            "remoteUser":"dev", "runArgs":["--cpus", "2", "--memory=512m"],
        }"#,
        );
        let plan = runtime.create_plan().unwrap();
        let args = args(&plan);
        assert!(args.contains(&format!(
            "type=bind,source={},target=/workspaces/{}",
            fixture.root.display(),
            fixture.root.file_name().unwrap().to_str().unwrap()
        )));
        assert!(args.contains(&"GREETING=two words $HOME; still literal".to_owned()));
        assert!(args.contains(&"--cpus=2".to_owned()));
        assert!(!args.contains(&"dev".to_owned()));
        assert!(runtime.config().working_folder().ends_with("/src"));
        assert_eq!(plan.program, PathBuf::from("docker"));
        assert!(plan.stdin.is_none());
    }

    #[test]
    fn explicit_mount_and_subfolder_variables() {
        let fixture = Fixture::new();
        let config = fixture.parse(r#"{"image":"alpine", "workspaceMount":"type=bind,source=${localWorkspaceFolder},target=/workspace,readonly,consistency=cached", "workspaceFolder":"${containerWorkspaceFolder}/code/${localWorkspaceFolderBasename}"}"#).unwrap();
        assert_eq!(config.workspace_mount().unwrap().target, "/workspace");
        assert!(config.workspace_mount().unwrap().readonly);
        assert_eq!(
            config.working_folder(),
            format!(
                "/workspace/code/{}",
                fixture.root.file_name().unwrap().to_str().unwrap()
            )
        );
    }

    #[test]
    fn dockerfile_build_and_legacy_paths() {
        let fixture = Fixture::new();
        for source in [
            r#"{"build":{"dockerfile":"Dockerfile", "context":"..", "target":"dev", "args":{"VALUE":"a b"}}}"#,
            r#"{"dockerFile":"Dockerfile", "context":".."}"#,
        ] {
            let runtime = fixture.runtime(source);
            let plan = runtime.build_plan().unwrap().unwrap();
            let args = args(&plan);
            assert!(
                args.contains(
                    &fixture
                        .root
                        .join(".devcontainer/Dockerfile")
                        .to_str()
                        .unwrap()
                        .to_owned()
                )
            );
            assert_eq!(args.last().unwrap(), fixture.root.to_str().unwrap());
            assert!(args.contains(&"solder-devcontainer/solder_test:local".to_owned()));
        }
        assert!(
            fixture
                .runtime(r#"{"image":"alpine"}"#)
                .build_plan()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn host_environment_is_explicit_and_container_environment_is_deferred() {
        let fixture = Fixture::new();
        let config = Config::parse(r#"{"image":"alpine", "containerEnv":{"LOCAL":"${localEnv:VALUE:fallback}"}, "remoteEnv":{"PATH":"${containerEnv:PATH}:/tools", "EMPTY":"${localEnv:MISSING}", "DEFAULT":"${containerEnv:MISSING:a:b}"}, "remoteUser":"dev:1000", "containerUser":"root"}"#, &fixture.file, &fixture.root, BTreeMap::from([("VALUE".into(), "a b $literal".into())])).unwrap();
        assert_eq!(config.container_env()["LOCAL"], "a b $literal");
        let runtime = Runtime::new(config, None, "solder_test").unwrap();
        let id = ContainerId::parse(ID).unwrap();
        let inspect = serde_json::json!([{"Id":ID,"State":{"Running":true},"Config":{"Env":["PATH=/usr/bin","WITH_EQUALS=a=b"],"User":"root"}}]);
        let container = runtime
            .container_from_inspect(&id, &serde_json::to_vec(&inspect).unwrap())
            .unwrap();
        assert_eq!(container.env()["PATH"], "/usr/bin:/tools");
        assert_eq!(container.env()["DEFAULT"], "a:b");
        assert_eq!(container.env()["EMPTY"], "");
        assert_eq!(container.remote_user(), Some("dev:1000"));
        let plan = runtime
            .exec_plan(
                &container,
                &["printf".into(), "a b; $HOME".into()],
                true,
                true,
            )
            .unwrap();
        let args = args(&plan);
        assert!(args.contains(&"PATH=/usr/bin:/tools".to_owned()));
        assert_eq!(&args[args.len() - 3..], &[ID, "printf", "a b; $HOME"]);
    }

    #[test]
    fn rejects_control_flags_and_unsupported_features() {
        let fixture = Fixture::new();
        for args in [
            vec!["--host=tcp://attacker"],
            vec!["-H", "attacker"],
            vec!["--context", "other"],
            vec!["--privileged"],
            vec!["--mount", "type=bind,source=/,target=/host"],
            vec!["-v", "/:/host"],
            vec!["--entrypoint", "bad"],
            vec!["alpine", "echo"],
            vec!["--name=existing"],
            vec!["--env-file", "/secret"],
            vec!["--cpus", "--privileged"],
            vec!["--memory", "1m --privileged"],
            vec!["--network=host"],
            vec!["--"],
        ] {
            let source = serde_json::json!({"image":"alpine","runArgs":args});
            assert!(fixture.parse(&source.to_string()).is_err(), "{source}");
        }
        for (key, value) in [
            ("features", serde_json::json!({})),
            ("mounts", serde_json::json!([])),
            ("initializeCommand", serde_json::json!("echo host")),
            ("updateRemoteUserUID", serde_json::json!(true)),
            ("postCreateCommand", serde_json::json!({"a":"echo a"})),
        ] {
            let mut source = serde_json::json!({"image":"alpine"});
            source[key] = value;
            assert!(matches!(
                fixture.parse(&source.to_string()),
                Err(Error::Unsupported(_))
            ));
        }
    }

    #[test]
    fn invalid_sources_env_and_container_paths() {
        let fixture = Fixture::new();
        for source in [
            r#"{}"#,
            r#"[]"#,
            r#"{"image":"--privileged"}"#,
            r#"{"image":"alpine","build":{"dockerfile":"Dockerfile"}}"#,
            r#"{"image":"alpine","workspaceFolder":"relative"}"#,
            r#"{"image":"alpine","workspaceFolder":"/workspaces/../etc"}"#,
            r#"{"image":"alpine","containerEnv":{"-e":"evil"}}"#,
            r#"{"image":"alpine","remoteEnv":{"PATH":null}}"#,
            r#"{"image":"alpine","containerEnv":{"X":"${containerEnv:PATH}"}}"#,
            r#"{"image":"alpine","workspaceFolder":"${unknown}"}"#,
            r#"{"image":"alpine","remoteUser":"--privileged"}"#,
            r#"{"image":"alpine","postCreateCommand":[]}"#,
            r#"{"image":"alpine","postCreateCommand":"echo ${localWorkspaceFolder}"}"#,
            r#"{"image":"alpine","workspaceMount":"type=volume,source=data,target=/workspace","workspaceFolder":"/workspace"}"#,
            r#"{"build":{"dockerfile":"Dockerfile","options":["--output=/host"]}}"#,
        ] {
            assert!(fixture.parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn outside_and_missing_host_paths_are_rejected() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        for source in [
            serde_json::json!({"build":{"dockerfile":outside.file}}),
            serde_json::json!({"build":{"dockerfile":"Dockerfile","context":outside.root}}),
            serde_json::json!({"dockerComposeFile":outside.file,"service":"app"}),
            serde_json::json!({"image":"alpine","workspaceMount":format!("type=bind,source={},target=/workspace",outside.root.display()),"workspaceFolder":"/workspace"}),
            serde_json::json!({"build":{"dockerfile":"missing"}}),
        ] {
            assert!(fixture.parse(&source.to_string()).is_err());
        }
        assert!(
            Config::parse(
                r#"{"image":"alpine"}"#,
                &outside.file,
                &fixture.root,
                BTreeMap::new()
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        std::os::unix::fs::symlink(&outside.root, fixture.root.join("escape")).unwrap();
        assert!(
            fixture
                .parse(r#"{"build":{"dockerfile":"../escape/.devcontainer/Dockerfile"}}"#)
                .is_err()
        );
        let mut model = fixture.model();
        model["services"]["app"]["volumes"][0]["source"] =
            fixture.root.join("escape").to_str().unwrap().into();
        assert!(
            fixture
                .compose()
                .accept_compose_json(&serde_json::to_vec(&model).unwrap())
                .is_err()
        );
    }

    #[test]
    fn compose_files_services_and_frozen_configuration() {
        let fixture = Fixture::new();
        let mut runtime = fixture.compose();
        let config_plan = runtime.compose_config_plan().unwrap().unwrap();
        let config_args = args(&config_plan);
        let first = config_args
            .iter()
            .position(|arg| arg.ends_with("compose base.yml"))
            .unwrap();
        let second = config_args
            .iter()
            .position(|arg| arg.ends_with("compose override.yml"))
            .unwrap();
        assert!(first < second);
        assert!(config_args.ends_with(&["config".into(), "--format".into(), "json".into()]));
        assert!(runtime.create_plan().is_err());
        runtime
            .accept_compose_json(&serde_json::to_vec(&fixture.model()).unwrap())
            .unwrap();
        let plan = runtime.create_plan().unwrap();
        let plan_args = args(&plan);
        assert!(plan_args.contains(&"--no-build".to_owned()));
        assert!(plan_args.ends_with(&["app".into(), "db".into()]));
        let model: Value = serde_json::from_slice(plan.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            model["services"]["app"]["environment"]["LITERAL"],
            "$$HOME $${UNSET}"
        );
        assert_eq!(
            model["services"]["app"]["volumes"][0]["bind"]["create_host_path"],
            false
        );
        assert!(args(&runtime.resolve_id_plan().unwrap()).ends_with(&[
            "ps".into(),
            "--all".into(),
            "--quiet".into(),
            "app".into()
        ]));
        assert!(plan.remove_env.contains(&"COMPOSE_FILE".to_owned()));
    }

    #[test]
    fn compose_rejects_missing_service_overrides_and_unsafe_models() {
        let fixture = Fixture::new();
        for source in [
            r#"{"dockerComposeFile":"../compose base.yml"}"#,
            r#"{"dockerComposeFile":"../compose base.yml","service":"--help"}"#,
            r#"{"dockerComposeFile":"../compose base.yml","service":"app","runServices":["db"]}"#,
            r#"{"dockerComposeFile":"../compose base.yml","service":"app","containerEnv":{"A":"b"}}"#,
        ] {
            assert!(fixture.parse(source).is_err());
        }
        for (key, value) in [
            ("privileged", Value::Bool(true)),
            ("network_mode", "host".into()),
            ("devices", serde_json::json!(["/dev/kvm"])),
            ("container_name", "existing".into()),
            ("env_file", "/secret".into()),
            ("ports", serde_json::json!(["80:80"])),
            ("deploy", serde_json::json!({"replicas":2})),
        ] {
            let mut model = fixture.model();
            model["services"]["db"][key] = value;
            assert!(
                fixture
                    .compose()
                    .accept_compose_json(&serde_json::to_vec(&model).unwrap())
                    .is_err(),
                "{key}"
            );
        }
        let mut model = fixture.model();
        model["services"].as_object_mut().unwrap().remove("app");
        assert!(
            fixture
                .compose()
                .accept_compose_json(&serde_json::to_vec(&model).unwrap())
                .is_err()
        );
        let mut model = fixture.model();
        model["volumes"]["data"]["driver_opts"] =
            serde_json::json!({"type":"none","device":"/","o":"bind"});
        assert!(
            fixture
                .compose()
                .accept_compose_json(&serde_json::to_vec(&model).unwrap())
                .is_err()
        );
        let mut model = fixture.model();
        model["volumes"]["data"]["external"] = true.into();
        assert!(
            fixture
                .compose()
                .accept_compose_json(&serde_json::to_vec(&model).unwrap())
                .is_err()
        );
    }

    #[test]
    fn compose_build_is_project_local() {
        let fixture = Fixture::new();
        let mut model = fixture.model();
        model["services"]["app"]["build"] = serde_json::json!({"context":fixture.root.join(".devcontainer"),"dockerfile":"Dockerfile","args":{"ONE":"two words"},"target":"dev"});
        let mut runtime = fixture.compose();
        runtime
            .accept_compose_json(&serde_json::to_vec(&model).unwrap())
            .unwrap();
        assert!(runtime.build_plan().unwrap().unwrap().stdin.is_some());
        model["services"]["app"]["build"]["context"] = "/".into();
        assert!(
            runtime
                .accept_compose_json(&serde_json::to_vec(&model).unwrap())
                .is_err()
        );
        assert!(runtime.build_plan().is_err());
    }

    #[test]
    fn compose_dependencies_and_ambient_controls_are_bounded() {
        let fixture = Fixture::new();
        let config = Config::parse(
            r#"{"dockerComposeFile":"../compose base.yml","service":"${localEnv:SERVICE:app}","runServices":["app"]}"#,
            &fixture.file, &fixture.root,
            BTreeMap::from([
                ("COMPOSE_REMOVE_ORPHANS".into(), "1".into()),
                ("COMPOSE_FILE".into(), "/outside.yml".into()),
                ("DOCKER_HOST".into(), "tcp://attacker".into()),
            ]),
        ).unwrap();
        let mut runtime = Runtime::new(config, None, "solder_test").unwrap();
        let plan = runtime.compose_config_plan().unwrap().unwrap();
        assert!(!plan.env.contains_key("DOCKER_HOST"));
        assert!(!plan.env.contains_key("COMPOSE_FILE"));
        assert_eq!(plan.env["COMPOSE_REMOVE_ORPHANS"], "0");
        runtime
            .accept_compose_json(&serde_json::to_vec(&fixture.model()).unwrap())
            .unwrap();
        let id = ContainerId::parse(ID).unwrap();
        for plan in [
            runtime.create_plan().unwrap(),
            runtime.start_plan(&id).unwrap(),
            runtime.stop_plan(&id).unwrap(),
        ] {
            assert!(args(&plan).ends_with(&["app".into(), "db".into()]));
        }
        let mut model = fixture.model();
        model["services"]["app"]["volumes"][0]["source"] = "/".into();
        assert!(
            runtime
                .accept_compose_json(&serde_json::to_vec(&model).unwrap())
                .is_err()
        );
        let mut runtime =
            fixture.runtime(r#"{"dockerComposeFile":"../compose base.yml","service":"app"}"#);
        runtime
            .accept_compose_json(&serde_json::to_vec(&fixture.model()).unwrap())
            .unwrap();
        assert_eq!(
            args(&runtime.create_plan().unwrap()).last().unwrap(),
            "--force-recreate"
        );
    }

    #[test]
    fn local_user_substitution_and_expansion_limits() {
        let fixture = Fixture::new();
        let config = Config::parse(r#"{"image":"alpine","remoteUser":"${localEnv:DEV_USER:dev}","updateRemoteUserUID":false,"userEnvProbe":"none"}"#, &fixture.file, &fixture.root, BTreeMap::new()).unwrap();
        let runtime = Runtime::new(config, None, "solder_test").unwrap();
        let id = ContainerId::parse(ID).unwrap();
        let inspect = serde_json::json!([{"Id":ID,"State":{"Running":true},"Config":{"Env":[],"User":"root"}}]);
        assert_eq!(
            runtime
                .container_from_inspect(&id, &serde_json::to_vec(&inspect).unwrap())
                .unwrap()
                .remote_user(),
            Some("dev")
        );
        let env = BTreeMap::from([("VALUE".into(), "a".repeat(MAX_CONFIG))]);
        assert!(
            Config::parse(
                r#"{"image":"alpine","containerEnv":{"X":"${localEnv:VALUE}${localEnv:VALUE}"}}"#,
                &fixture.file,
                &fixture.root,
                env
            )
            .is_err()
        );
    }

    #[test]
    fn runner_native_operations_and_raw_error_output() {
        let fixture = Fixture::new();
        let runtime = fixture.runtime(
            r#"{"image":"alpine","postCreateCommand":["echo","a b"],"postStartCommand":"echo ok"}"#,
        );
        let mut operations = Vec::new();
        let mut runner = |plan: &CommandPlan| {
            operations.push(plan.operation);
            match plan.operation {
                Operation::Create => success(format!("{ID}\n").into_bytes()),
                Operation::Inspect => success(serde_json::to_vec(&serde_json::json!([{"Id":ID,"State":{"Running":true},"Config":{"Env":[],"User":""}}])).unwrap()),
                _ => success(Vec::new()),
            }
        };
        runtime.build(&mut runner).unwrap();
        let id = runtime.create(&mut runner).unwrap();
        let container = runtime.start(&id, &mut runner).unwrap();
        runtime
            .run_lifecycle(
                &container,
                &[Lifecycle::PostCreate, Lifecycle::PostStart],
                &mut runner,
            )
            .unwrap();
        runtime.stop(&id, &mut runner).unwrap();
        assert_eq!(
            operations,
            [
                Operation::Create,
                Operation::Start,
                Operation::Inspect,
                Operation::Exec,
                Operation::Exec,
                Operation::Stop
            ]
        );
        let mut count = 0;
        let mut failing = |_: &CommandPlan| {
            count += 1;
            Ok(CommandOutput {
                code: Some(17),
                stdout: b"partial output".to_vec(),
                stderr: b"daemon: permission denied\n".to_vec(),
            })
        };
        let error = runtime
            .run_lifecycle(
                &container,
                &[Lifecycle::PostCreate, Lifecycle::PostStart],
                &mut failing,
            )
            .unwrap_err();
        assert_eq!(count, 1);
        assert!(
            matches!(error, Error::Command { operation: Operation::Exec, code: Some(17), ref stderr, .. } if stderr == "daemon: permission denied\n")
        );
        let mut missing = |_: &CommandPlan| -> io::Result<CommandOutput> {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Docker executable is missing",
            ))
        };
        assert!(matches!(runtime.create(&mut missing), Err(Error::Io(_))));
    }

    #[test]
    fn runner_compose_operation_order_and_id_protocol() {
        let fixture = Fixture::new();
        let mut runtime = fixture.compose();
        let model = serde_json::to_vec(&fixture.model()).unwrap();
        let mut operations = Vec::new();
        let mut runner = |plan: &CommandPlan| {
            operations.push(plan.operation);
            match plan.operation {
                Operation::ComposeConfig => success(model.clone()),
                Operation::ResolveId => success(format!("{ID}\n").into_bytes()),
                _ => success(Vec::new()),
            }
        };
        runtime.resolve_compose(&mut runner).unwrap();
        runtime.build(&mut runner).unwrap();
        let id = runtime.create(&mut runner).unwrap();
        runtime.stop(&id, &mut runner).unwrap();
        assert_eq!(
            operations,
            [
                Operation::ComposeConfig,
                Operation::Build,
                Operation::Create,
                Operation::ResolveId,
                Operation::Stop
            ]
        );
        for output in [
            "",
            "warning: started",
            "--help",
            "abc123",
            &format!("{ID}\n{ID}\n"),
        ] {
            assert!(ContainerId::parse(output).is_err());
        }
    }

    #[test]
    fn inspect_identity_running_state_and_metadata_are_checked() {
        let fixture = Fixture::new();
        let runtime = fixture.runtime(r#"{"image":"alpine"}"#);
        let id = ContainerId::parse(ID).unwrap();
        let mut inspect = serde_json::json!([{"Id":ID,"State":{"Running":false},"Config":{"Env":null,"User":"1000"}}]);
        let container = runtime
            .container_from_inspect(&id, &serde_json::to_vec(&inspect).unwrap())
            .unwrap();
        assert!(!container.running());
        assert!(
            runtime
                .exec_plan(&container, &["sh".into()], false, false)
                .is_err()
        );
        inspect[0]["Config"]["Labels"] = serde_json::json!({"devcontainer.metadata":"[]"});
        assert!(matches!(
            runtime.container_from_inspect(&id, &serde_json::to_vec(&inspect).unwrap()),
            Err(Error::Unsupported(_))
        ));
        assert!(runtime.container_from_inspect(&id, b"[]").is_err());
        assert!(
            runtime
                .container_from_inspect(&id, b"Docker daemon unavailable")
                .is_err()
        );
        inspect[0]["Id"] = "a".repeat(64).into();
        assert!(
            runtime
                .container_from_inspect(&id, &serde_json::to_vec(&inspect).unwrap())
                .is_err()
        );
    }

    #[test]
    fn discovery_and_size_limits() {
        let fixture = Fixture::new();
        assert_eq!(discover(&fixture.root).unwrap(), Some(fixture.file.clone()));
        std::fs::remove_file(&fixture.file).unwrap();
        let fallback = fixture.root.join(".devcontainer.json");
        std::fs::write(&fallback, "{\"image\":\"alpine\"}").unwrap();
        assert_eq!(discover(&fixture.root).unwrap(), Some(fallback.clone()));
        assert!(Config::load(&fallback, &fixture.root, BTreeMap::new()).is_ok());
        std::fs::write(&fallback, " ".repeat(MAX_CONFIG + 1)).unwrap();
        assert!(Config::load(&fallback, &fixture.root, BTreeMap::new()).is_err());
        std::fs::remove_file(&fallback).unwrap();
        assert_eq!(discover(&fixture.root).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn real_fake_docker_executable_receives_literal_arguments() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let program = fixture.root.join("fake docker");
        std::fs::write(
            &program,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$RECORD\"\nprintf '%s\\n' '{ID}'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let runtime = Runtime::new(fixture.parse(r#"{"image":"alpine","containerEnv":{"MESSAGE":"a b; $(touch /not-a-command)"}}"#).unwrap(), Some(program), "solder_test").unwrap();
        let record = fixture.root.join("record");
        let mut runner = |plan: &CommandPlan| {
            let mut command = std::process::Command::new(&plan.program);
            command
                .args(&plan.args)
                .current_dir(&plan.cwd)
                .env("RECORD", &record);
            for key in &plan.remove_env {
                command.env_remove(key);
            }
            command.envs(&plan.env);
            let output = command.output()?;
            Ok(CommandOutput {
                code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        };
        assert_eq!(runtime.create(&mut runner).unwrap().as_str(), ID);
        let record = std::fs::read_to_string(record).unwrap();
        assert!(
            record
                .lines()
                .any(|line| line == "MESSAGE=a b; $(touch /not-a-command)")
        );
        assert!(
            record
                .lines()
                .any(|line| line.contains("type=bind,source=") && line.contains(" $literal"))
        );
    }
}
