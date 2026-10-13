//! Remote actions and their blocking preparation. Parent integration is listed
//! in ui-integrations.md. Cache recognition never probes a local project.

pub(crate) mod execution {
    use std::{
        io::{self, Read, Write},
        path::{Component, Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicBool, AtomicU64, Ordering},
            mpsc,
        },
        time::{Duration, Instant, SystemTime},
    };

    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    use crate::{
        devcontainer::{
            self, CommandOutput, CommandPlan, Config, ContainerId, Lifecycle, Operation, Runner,
            Runtime, ShutdownAction, Source,
        },
        remote::{self, Endpoint, Mount, Target},
        remote_ui,
    };

    const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
    const REFERENCE_LIMIT: usize = 8 * 1024 * 1024;
    const CONNECTION_LIMIT: usize = 16 * 1024;

    fn invalid(message: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidInput, message)
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Connection {
        version: u8,
        host: String,
        port: u16,
        path: String,
    }

    impl Connection {
        fn from_target(target: &Target) -> io::Result<Self> {
            let Endpoint::Ssh { host, port } = &target.endpoint else {
                return Err(invalid("Expected an SSH connection"));
            };
            remote_ui::parse_target(&format!("ssh://{host}:{port}/"))
                .map_err(|_| invalid("Invalid SSH connection"))?;
            if !target.path.starts_with('/') || target.path.chars().any(char::is_control) {
                return Err(invalid("Invalid SSH project path"));
            }
            let connection = Self {
                version: 1,
                host: host.clone(),
                port: *port,
                path: target.path.clone(),
            };
            if connection.bytes()?.len() > CONNECTION_LIMIT {
                return Err(invalid("SSH connection metadata is too large"));
            }
            Ok(connection)
        }
        fn bytes(&self) -> io::Result<Vec<u8>> {
            serde_json::to_vec(self).map_err(io::Error::other)
        }
        fn target(&self) -> io::Result<Target> {
            if self.version != 1 {
                return Err(invalid("Unsupported SSH connection metadata"));
            }
            let target = Target {
                endpoint: Endpoint::Ssh {
                    host: self.host.clone(),
                    port: self.port,
                },
                path: self.path.clone(),
            };
            Self::from_target(&target)?;
            Ok(target)
        }
        fn namespace(&self, settings: &Path) -> io::Result<PathBuf> {
            Ok(settings
                .join("remote/ssh")
                .join(ai::install::sha256_hex(&self.bytes()?)))
        }
    }

    /// Exactly our namespace, with no IO, normalization or marker-file lookup.
    pub fn cached_under(settings: &Path, root: &Path) -> bool {
        remote::cache_root_under(settings, root).as_deref() == Some(root)
            && !root
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    }

    fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(invalid("Configuration reference is too large"));
        }
        Ok(bytes)
    }

    fn read_connection(file: &Path) -> io::Result<Connection> {
        if std::fs::symlink_metadata(file)?.is_symlink() {
            return Err(invalid("SSH metadata must not be a symlink"));
        }
        let connection: Connection = serde_json::from_slice(&read_bounded(file, CONNECTION_LIMIT)?)
            .map_err(io::Error::other)?;
        connection.target()?;
        Ok(connection)
    }

    fn write_connection(file: &Path, connection: &Connection) -> io::Result<()> {
        match read_connection(file) {
            Ok(existing) if existing == *connection => return Ok(()),
            Ok(_) => return Err(invalid("SSH cache belongs to another connection")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = file.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            output.write_all(&connection.bytes()?)?;
            output.sync_all()?;
            std::fs::rename(&temporary, file)
        })();
        if result.is_err() {
            std::fs::remove_file(&temporary).ok();
        }
        result
    }

    pub fn connect_under(settings: &Path, target: Target) -> io::Result<Arc<Mount>> {
        // Two windows can request the same cache while their SSH handshakes run.
        // Register once; replacing a live registration would strand its views.
        static CONNECT: OnceLock<Mutex<()>> = OnceLock::new();
        let _connecting = CONNECT
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| io::Error::other("SSH connection lock failed"))?;
        let connection = Connection::from_target(&target)?;
        let namespace = connection.namespace(settings)?;
        check_namespace(settings, &namespace)?;
        std::fs::create_dir_all(&namespace)?;
        write_connection(&namespace.join("connection.json"), &connection)?;
        let root = namespace.join("project");
        if let Ok(Some(mount)) = remote::mount(&root)
            && mount.is_connected()
        {
            return Ok(mount);
        }
        let mut target = target;
        if let Endpoint::Ssh { host, .. } = &mut target.endpoint {
            // URI brackets delimit IPv6 in the picker; ssh's host argv is plain.
            *host = host.replace(['[', ']'], "");
        }
        Mount::connect(target, root, true)
    }

    pub fn prepare_under(settings: &Path, root: &Path) -> io::Result<Option<Arc<Mount>>> {
        if !cached_under(settings, root) {
            return Ok(None);
        }
        check_namespace(
            settings,
            root.parent().ok_or_else(|| invalid("Invalid SSH cache"))?,
        )?;
        let connection = read_connection(
            &root
                .parent()
                .ok_or_else(|| invalid("Invalid SSH cache"))?
                .join("connection.json"),
        )?;
        if connection.namespace(settings)?.join("project") != root {
            return Err(invalid("SSH cache metadata does not match its namespace"));
        }
        connect_under(settings, connection.target()?).map(Some)
    }

    fn check_namespace(settings: &Path, namespace: &Path) -> io::Result<()> {
        // Canonical roots are persisted by windows. Refuse linked ancestors
        // so that a cache can never acquire a second, unrecognized identity.
        for path in settings.ancestors().map(Path::to_path_buf).chain([
            settings.join("remote"),
            settings.join("remote/ssh"),
            namespace.to_owned(),
            namespace.join("project"),
        ]) {
            match std::fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_symlink() || !metadata.is_dir() => {
                    return Err(invalid("SSH cache directories must not be symlinks"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Dropping the foreground task cancels its still-running background job.
    pub struct Cancellation(pub Arc<AtomicBool>);
    impl Cancellation {
        pub fn new() -> Self {
            Self(Arc::new(AtomicBool::new(false)))
        }
    }
    impl Drop for Cancellation {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn cancelled(flag: &AtomicBool) -> io::Result<()> {
        if flag.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Project operation cancelled",
            ))
        } else {
            Ok(())
        }
    }

    pub struct BoundedRunner {
        cancel: Arc<AtomicBool>,
        timeout: Duration,
        limit: usize,
    }
    impl BoundedRunner {
        pub fn new(cancel: Arc<AtomicBool>) -> Self {
            Self {
                cancel,
                timeout: Duration::from_secs(300),
                limit: OUTPUT_LIMIT,
            }
        }
    }

    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            // A shell can leave descendants holding its pipes after it exits.
            // Give each CLI a process group and end that group on all exits.
            #[cfg(unix)]
            {
                Command::new("/bin/kill")
                    .args(["-KILL", "--", &format!("-{}", self.0.id())])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .ok();
            }
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }

    enum Stream {
        Stdout(io::Result<Vec<u8>>),
        Stderr(io::Result<Vec<u8>>),
        Stdin(io::Result<()>),
    }
    fn capture(mut input: impl Read, limit: usize) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        input
            .by_ref()
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(invalid("Docker output exceeds its limit"));
        }
        Ok(bytes)
    }

    impl Runner for BoundedRunner {
        fn run(&mut self, plan: &CommandPlan) -> io::Result<CommandOutput> {
            cancelled(&self.cancel)?;
            let mut command = Command::new(&plan.program);
            command
                .args(&plan.args)
                .current_dir(&plan.cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for key in &plan.remove_env {
                command.env_remove(key);
            }
            command.envs(&plan.env);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            let mut process = Process(command.spawn()?);
            let mut input = process
                .0
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("Missing Docker stdin"))?;
            let output = process
                .0
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("Missing Docker stdout"))?;
            let errors = process
                .0
                .stderr
                .take()
                .ok_or_else(|| io::Error::other("Missing Docker stderr"))?;
            let (tx, rx) = mpsc::channel();
            let bytes = plan.stdin.clone();
            let writer = tx.clone();
            std::thread::Builder::new()
                .name("container-input".into())
                .spawn(move || {
                    let result = bytes.map_or(Ok(()), |bytes| input.write_all(&bytes));
                    drop(input);
                    writer.send(Stream::Stdin(result)).ok();
                })?;
            let stdout = tx.clone();
            let limit = self.limit;
            std::thread::Builder::new()
                .name("container-output".into())
                .spawn(move || {
                    stdout.send(Stream::Stdout(capture(output, limit))).ok();
                })?;
            std::thread::Builder::new()
                .name("container-errors".into())
                .spawn(move || {
                    tx.send(Stream::Stderr(capture(errors, limit))).ok();
                })?;
            let deadline = Instant::now() + self.timeout;
            let mut result = CommandOutput::default();
            let mut streams = 0;
            let mut exited = false;
            loop {
                cancelled(&self.cancel)?;
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Docker operation timed out",
                    ));
                }
                if !exited && let Some(status) = process.0.try_wait()? {
                    result.code = status.code();
                    exited = true;
                }
                if exited && streams == 3 {
                    return Ok(result);
                }
                match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(Stream::Stdout(bytes)) => {
                        result.stdout = bytes?;
                        streams += 1;
                    }
                    Ok(Stream::Stderr(bytes)) => {
                        result.stderr = bytes?;
                        streams += 1;
                    }
                    Ok(Stream::Stdin(written)) => {
                        written?;
                        streams += 1;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) if streams == 3 => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => return Err(io::Error::other("Docker output reader failed")),
                }
            }
        }
    }

    #[derive(Debug)]
    pub enum Failure {
        NoConfig,
        Changed,
        Bind,
        ExtraMount,
        SharedFiles,
        Runtime(devcontainer::Error),
        Io(io::Error),
    }
    impl From<io::Error> for Failure {
        fn from(error: io::Error) -> Self {
            Self::Io(error)
        }
    }
    impl From<devcontainer::Error> for Failure {
        fn from(error: devcontainer::Error) -> Self {
            Self::Runtime(error)
        }
    }

    #[derive(Clone)]
    struct Reference {
        path: PathBuf,
        bytes: Option<Vec<u8>>,
    }
    impl Reference {
        fn read(path: PathBuf) -> io::Result<Self> {
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_symlink() => {
                    return Err(invalid("Configuration references must not be symlinks"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let bytes = match read_bounded(&path, REFERENCE_LIMIT) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            Ok(Self { path, bytes })
        }
        fn unchanged(&self) -> bool {
            Self::read(self.path.clone()).is_ok_and(|now| now.bytes == self.bytes)
        }
    }

    pub struct Reviewed {
        pub config: Config,
        pub details: Value,
        references: Vec<Reference>,
    }
    impl Reviewed {
        pub fn load(root: &Path) -> Result<Self, Failure> {
            let file = devcontainer::discover(root)?.ok_or(Failure::NoConfig)?;
            let reference = Reference::read(file.clone())?;
            let source = std::str::from_utf8(reference.bytes.as_deref().ok_or(Failure::Changed)?)
                .map_err(io::Error::other)?;
            let details = import::jsonc::parse(source).map_err(io::Error::other)?;
            // Keep substitution inputs only in memory, never in project metadata.
            let config = Config::load(&file, root, std::env::vars().collect())?;
            if !reference.unchanged() {
                return Err(Failure::Changed);
            }
            let mut references = vec![reference, Reference::read(config.workspace().join(".env"))?];
            match config.source() {
                Source::Dockerfile {
                    dockerfile,
                    context,
                    ..
                } => {
                    references.push(Reference::read(dockerfile.clone())?);
                    references.push(Reference::read(context.join(".dockerignore"))?);
                    references.push(Reference::read(dockerfile.with_file_name(format!(
                        "{}.dockerignore",
                        dockerfile.file_name().unwrap_or_default().to_string_lossy()
                    )))?);
                }
                Source::Compose { files, .. } => {
                    for file in files {
                        references.push(Reference::read(file.clone())?);
                    }
                }
                Source::Image(_) => {}
            }
            if references.iter().any(|reference| !reference.unchanged()) {
                return Err(Failure::Changed);
            }
            Ok(Self {
                config,
                details,
                references,
            })
        }
        fn check(&self) -> Result<(), Failure> {
            if self.references.iter().all(Reference::unchanged) {
                Ok(())
            } else {
                Err(Failure::Changed)
            }
        }
    }

    fn physical_bind(config: &Config, model: Option<&Value>) -> Result<String, Failure> {
        let folder = config.working_folder();
        if let Some(mount) = config.workspace_mount() {
            if mount.source != config.workspace() || mount.target != folder || mount.readonly {
                return Err(Failure::Bind);
            }
            return Ok(mount.target.clone());
        }
        let Source::Compose { service, .. } = config.source() else {
            return Err(Failure::Bind);
        };
        let services = model
            .and_then(|model| model["services"].as_object())
            .ok_or(Failure::Bind)?;
        let mut found = false;
        for (name, entry) in services {
            let volumes = entry
                .get("volumes")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice);
            for volume in volumes {
                if name != service || found {
                    return Err(Failure::ExtraMount);
                }
                if volume["type"] != "bind"
                    || volume["source"].as_str().map(Path::new) != Some(config.workspace())
                    || volume["target"] != folder
                    || volume["read_only"].as_bool() == Some(true)
                {
                    return Err(Failure::Bind);
                }
                found = true;
            }
        }
        if !found {
            return Err(Failure::Bind);
        }
        Ok(folder.to_owned())
    }

    fn inspected_bind(json: &[u8], root: &Path, destination: &str) -> Result<(), Failure> {
        let inspect: Value = serde_json::from_slice(json).map_err(io::Error::other)?;
        let entries = inspect
            .as_array()
            .filter(|entries| entries.len() == 1)
            .ok_or(Failure::Bind)?;
        let mounts = entries[0]["Mounts"]
            .as_array()
            .filter(|mounts| mounts.len() == 1)
            .ok_or(Failure::ExtraMount)?;
        let mount = &mounts[0];
        if mount["Type"] != "bind"
            || mount["Source"].as_str().map(Path::new) != Some(root)
            || mount["Destination"] != destination
            || mount["RW"] != true
        {
            return Err(Failure::Bind);
        }
        Ok(())
    }

    /// Inspect reports the daemon's paths. A file challenge also proves
    /// this client shares those bytes, including with a remote Docker context.
    fn verify_shared_files(
        runtime: &Runtime,
        container: &devcontainer::Container,
        runner: &mut impl Runner,
    ) -> Result<(), Failure> {
        let seed = format!("{}-{:?}", std::process::id(), SystemTime::now());
        let token = ai::install::sha256_hex(seed.as_bytes());
        let name = format!(".solder-bind-{}", &token[..24]);
        let path = runtime.config().workspace().join(&name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o644);
        }
        let mut file = options.open(&path)?;
        struct Probe(std::path::PathBuf);
        impl Drop for Probe {
            fn drop(&mut self) {
                std::fs::remove_file(&self.0).ok();
            }
        }
        let _probe = Probe(path);
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
        let target = format!(
            "{}/{}",
            container.working_folder().trim_end_matches('/'),
            name
        );
        let output =
            runner.run(&runtime.exec_plan(container, &["cat".into(), target], false, false)?)?;
        if output.code != Some(0) || output.stdout != token.as_bytes() {
            return Err(Failure::SharedFiles);
        }
        Ok(())
    }

    /// Only containers created by this operation enter this owner. A failed
    /// setup is stopped even when shutdownAction is none. No remove/prune.
    pub struct Managed {
        runtime: Option<Runtime>,
        id: ContainerId,
        stop: bool,
    }
    impl Managed {
        pub fn activated(&mut self) {
            self.stop = self
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.config().shutdown_action() != ShutdownAction::None);
        }
    }
    impl Drop for Managed {
        fn drop(&mut self) {
            let Some(runtime) = self.runtime.take().filter(|_| self.stop) else {
                return;
            };
            let id = self.id.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("container-shutdown".into())
                .spawn(move || {
                    let mut runner = BoundedRunner::new(Arc::new(AtomicBool::new(false)));
                    runner.timeout = Duration::from_secs(30);
                    if let Err(error) = runtime.stop(&id, &mut runner) {
                        crate::journal::error(format_args!(
                            "devcontainer shutdown failed: {error}"
                        ));
                    }
                })
            {
                crate::journal::error(format_args!(
                    "could not schedule devcontainer shutdown: {error}"
                ));
            }
        }
    }

    pub struct Attached {
        pub mount: Arc<Mount>,
        pub managed: Managed,
    }

    pub fn attach(
        reviewed: Reviewed,
        cancel: Arc<AtomicBool>,
        docker: Option<PathBuf>,
    ) -> Result<Attached, Failure> {
        cancelled(&cancel)?;
        reviewed.check()?;
        let root = reviewed.config.workspace().to_path_buf();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // An ephemeral instance never adopts containers left by another run.
        let instance = format!(
            "solder_{}_{}_{:x}_{}",
            &ai::install::sha256_hex(root.as_os_str().as_encoded_bytes())[..16],
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let mut runtime = Runtime::new(reviewed.config.clone(), docker.clone(), &instance)?;
        let mut runner = BoundedRunner::new(cancel.clone());
        let mut compose = None;
        runtime.resolve_compose(&mut |plan: &CommandPlan| {
            let output = runner.run(plan)?;
            if plan.operation == Operation::ComposeConfig && output.code == Some(0) {
                compose = Some(
                    serde_json::from_slice::<Value>(&output.stdout).map_err(io::Error::other)?,
                );
            }
            Ok(output)
        })?;
        let destination = physical_bind(runtime.config(), compose.as_ref())?;
        reviewed.check()?;
        runtime.build(&mut runner)?;
        reviewed.check()?;
        let id = runtime.create(&mut runner)?;
        let managed = Managed {
            runtime: Some(runtime),
            id,
            stop: true,
        };
        let runtime = managed.runtime.as_ref().expect("managed runtime");
        let container = runtime.start(&managed.id, &mut runner)?;
        let inspection = runner.run(&runtime.inspect_plan(&managed.id))?;
        if inspection.code != Some(0) {
            return Err(Failure::Bind);
        }
        runtime.container_from_inspect(&managed.id, &inspection.stdout)?;
        inspected_bind(&inspection.stdout, &root, &destination)?;
        verify_shared_files(runtime, &container, &mut runner)?;
        reviewed.check()?;
        runtime.run_lifecycle(
            &container,
            &[
                Lifecycle::OnCreate,
                Lifecycle::UpdateContent,
                Lifecycle::PostCreate,
                Lifecycle::PostStart,
                Lifecycle::PostAttach,
            ],
            &mut runner,
        )?;
        cancelled(&cancel)?;
        let target = Target {
            endpoint: Endpoint::Container {
                program: docker.unwrap_or_else(|| PathBuf::from("docker")),
                id: managed.id.as_str().to_owned(),
                user: container.remote_user().map(str::to_owned),
                env: container
                    .env()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            },
            path: destination,
        };
        let mount = Mount::connect(target, root, false)?;
        cancelled(&cancel)?;
        Ok(Attached { mount, managed })
    }

    pub fn same_environment(old: &Mount, new: &Mount) -> bool {
        if old.root != new.root || old.target.path != new.target.path {
            return false;
        }
        match (&old.target.endpoint, &new.target.endpoint) {
            (Endpoint::Ssh { host: a, port: ap }, Endpoint::Ssh { host: b, port: bp }) => {
                a == b && ap == bp
            }
            (
                Endpoint::Container {
                    program: a,
                    id: ai,
                    user: au,
                    env: ae,
                },
                Endpoint::Container {
                    program: b,
                    id: bi,
                    user: bu,
                    env: be,
                },
            ) => a == b && ai == bi && au == bu && ae == be,
            _ => false,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::BTreeMap;

        struct Fixture(PathBuf);
        impl Fixture {
            fn new() -> Self {
                static NEXT: AtomicU64 = AtomicU64::new(0);
                let root = std::env::temp_dir().join(format!(
                    "solder ui bridge {} {}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir(&root).unwrap();
                Self(root.canonicalize().unwrap())
            }
            fn config(&self, source: &str) -> Reviewed {
                std::fs::write(self.0.join(".devcontainer.json"), source).unwrap();
                Reviewed::load(&self.0).unwrap()
            }
            fn plan(&self, script: &str) -> CommandPlan {
                CommandPlan {
                    operation: Operation::Exec,
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), script.into(), "test-script".into()],
                    cwd: self.0.clone(),
                    stdin: None,
                    remove_env: Vec::new(),
                    env: BTreeMap::new(),
                }
            }
            #[cfg(unix)]
            fn executable(&self, name: &str, script: &str) -> PathBuf {
                use std::os::unix::fs::PermissionsExt;
                let path = self.0.join(name);
                std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
                path
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).ok();
            }
        }

        fn runner() -> BoundedRunner {
            BoundedRunner::new(Arc::new(AtomicBool::new(false)))
        }

        #[test]
        fn cache_predicate_is_strict_and_local_prepare_does_no_marker_io() {
            let fixture = Fixture::new();
            let key = "a".repeat(64);
            let root = fixture.0.join("remote/ssh").join(&key).join("project");
            assert!(cached_under(&fixture.0, &root));
            for relative in [
                format!("remote/ssh/{key}"),
                format!("remote/ssh/{key}/project/file"),
                format!("remote/ssh/{}/project", "A".repeat(64)),
                "remote/ssh/x/project".into(),
                format!("remote/ssh/{key}/project/../project"),
                format!("remote/ssh/{key}/other"),
            ] {
                assert!(!cached_under(&fixture.0, &fixture.0.join(relative)));
            }
            assert!(
                prepare_under(
                    &fixture.0,
                    &fixture.0.join("local project that does not exist")
                )
                .unwrap()
                .is_none()
            );
        }

        #[cfg(unix)]
        #[test]
        fn a_linked_settings_ancestor_cannot_change_the_cache_identity() {
            let fixture = Fixture::new();
            let real = fixture.0.join("real");
            std::fs::create_dir_all(real.join("settings")).unwrap();
            let linked = fixture.0.join("linked");
            std::os::unix::fs::symlink(&real, &linked).unwrap();
            for settings in [linked.join("settings"), linked] {
                let namespace = settings.join("remote/ssh").join("a".repeat(64));
                assert!(check_namespace(&settings, &namespace).is_err());
                assert!(!real.join("settings/remote").exists());
            }
            assert!(
                check_namespace(
                    &real.join("settings"),
                    &real.join("settings/remote/ssh").join("a".repeat(64))
                )
                .is_ok()
            );
        }

        #[test]
        fn connection_metadata_is_stable_plain_and_checked_before_connecting() {
            let fixture = Fixture::new();
            let target =
                remote_ui::parse_target("ssh://user@example.test:2222/project/a%20b").unwrap();
            let connection = Connection::from_target(&target).unwrap();
            let namespace = connection.namespace(&fixture.0).unwrap();
            assert_eq!(connection.namespace(&fixture.0).unwrap(), namespace);
            std::fs::create_dir_all(&namespace).unwrap();
            let file = namespace.join("connection.json");
            write_connection(&file, &connection).unwrap();
            let value: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            assert_eq!(value.as_object().unwrap().len(), 4);
            assert_eq!(value["host"], "user@example.test");
            let mut other = connection.clone();
            other.path = "/different".into();
            assert!(write_connection(&file, &other).is_err());
            std::fs::write(&file, other.bytes().unwrap()).unwrap();
            assert!(prepare_under(&fixture.0, &namespace.join("project")).is_err());
            std::fs::write(&file, br#"{"version":1,"host":"example.test","port":22,"path":"/project","env":{"TOKEN":"secret"}}"#).unwrap();
            assert!(read_connection(&file).is_err());
        }

        #[test]
        fn runner_preserves_argv_stdin_cwd_and_environment_removal() {
            let fixture = Fixture::new();
            let mut plan = fixture.plan("test -z \"${HOME+x}\" && test \"$PWD\" = \"$EXPECTED_CWD\" && printf '%s' \"$1\" && cat && printf '%s' \"$TOKEN\" >&2");
            plan.args.push("a b; $(never-run) 'quoted'".into());
            plan.stdin = Some(vec![b'x'; 64 * 1024]);
            plan.remove_env.push("HOME".into());
            plan.env
                .insert("EXPECTED_CWD".into(), fixture.0.to_str().unwrap().into());
            plan.env.insert("TOKEN".into(), "test value".into());
            let output = runner().run(&plan).unwrap();
            assert_eq!(output.code, Some(0));
            assert!(output.stdout.starts_with(b"a b; $(never-run) 'quoted'"));
            assert_eq!(
                output.stdout.len(),
                b"a b; $(never-run) 'quoted'".len() + 64 * 1024
            );
            assert_eq!(output.stderr, b"test value");
        }

        #[test]
        fn runner_bounds_both_streams_and_failed_exit_codes() {
            let fixture = Fixture::new();
            for stream in ["", " >&2"] {
                let mut runner = runner();
                runner.limit = 64;
                let output = runner.run(&fixture.plan(&format!(
                    "i=0; while test $i -lt 100; do printf abcdefgh{stream}; i=$((i+1)); done"
                )));
                assert_eq!(output.unwrap_err().kind(), io::ErrorKind::InvalidInput);
            }
            let output = runner()
                .run(&fixture.plan("printf failure >&2; exit 7"))
                .unwrap();
            assert_eq!(output.code, Some(7));
            assert_eq!(output.stderr, b"failure");
        }

        #[test]
        fn timeout_and_cancellation_end_processes_without_waiting_for_pipe_eof() {
            let fixture = Fixture::new();
            let plan = fixture.plan("sleep 3 & wait");
            let mut runner = runner();
            runner.timeout = Duration::from_millis(80);
            let started = Instant::now();
            assert_eq!(
                runner.run(&plan).unwrap_err().kind(),
                io::ErrorKind::TimedOut
            );
            assert!(started.elapsed() < Duration::from_secs(2));
            let flag = runner.cancel.clone();
            runner.timeout = Duration::from_secs(3);
            let cancel = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(60));
                flag.store(true, Ordering::Release);
            });
            assert_eq!(
                runner.run(&plan).unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
            cancel.join().unwrap();
        }

        #[test]
        fn configuration_and_reference_changes_invalidate_approval() {
            let fixture = Fixture::new();
            std::fs::write(fixture.0.join("Dockerfile"), "FROM existing:test").unwrap();
            let reviewed = fixture.config(r#"{"build":{"dockerfile":"Dockerfile","context":"."}}"#);
            assert!(reviewed.check().is_ok());
            std::fs::write(fixture.0.join("Dockerfile"), "FROM changed:test").unwrap();
            assert!(matches!(reviewed.check(), Err(Failure::Changed)));
            let reviewed = fixture.config(r#"{"image":"existing:test"}"#);
            std::fs::write(fixture.0.join(".env"), "TOKEN=changed").unwrap();
            assert!(matches!(reviewed.check(), Err(Failure::Changed)));
            let reviewed = fixture.config(r#"{"image":"existing:test"}"#);
            std::fs::write(
                fixture.0.join(".devcontainer.json"),
                r#"{"image":"changed:test"}"#,
            )
            .unwrap();
            assert!(matches!(reviewed.check(), Err(Failure::Changed)));
        }

        #[test]
        fn physical_mapping_rejects_readonly_subfolders_and_extra_mounts() {
            let fixture = Fixture::new();
            let reviewed = fixture.config(r#"{"image":"existing:test"}"#);
            let folder = reviewed.config.working_folder();
            let mount = serde_json::json!([{"Mounts":[{"Type":"bind","Source":fixture.0,"Destination":folder,"RW":true}]}]);
            assert!(
                inspected_bind(&serde_json::to_vec(&mount).unwrap(), &fixture.0, folder).is_ok()
            );
            let mut readonly = mount.clone();
            readonly[0]["Mounts"][0]["RW"] = false.into();
            assert!(matches!(
                inspected_bind(&serde_json::to_vec(&readonly).unwrap(), &fixture.0, folder),
                Err(Failure::Bind)
            ));
            let mut extra = mount.clone();
            extra[0]["Mounts"]
                .as_array_mut()
                .unwrap()
                .push(mount[0]["Mounts"][0].clone());
            assert!(matches!(
                inspected_bind(&serde_json::to_vec(&extra).unwrap(), &fixture.0, folder),
                Err(Failure::ExtraMount)
            ));
            let readonly = fixture.config(r#"{"image":"existing:test","workspaceMount":"type=bind,source=${localWorkspaceFolder},target=/work,readonly","workspaceFolder":"/work"}"#);
            assert!(matches!(
                physical_bind(&readonly.config, None),
                Err(Failure::Bind)
            ));
            let nested = fixture.config(r#"{"image":"existing:test","workspaceFolder":"${containerWorkspaceFolder}/nested"}"#);
            assert!(matches!(
                physical_bind(&nested.config, None),
                Err(Failure::Bind)
            ));
            std::fs::write(fixture.0.join("compose.yaml"), "services: {}").unwrap();
            let compose = fixture.config(
                r#"{"dockerComposeFile":"compose.yaml","service":"app","workspaceFolder":"/work"}"#,
            );
            let model = serde_json::json!({"services":{"app":{"volumes":[{"type":"bind","source":fixture.0,"target":"/work"}]},"db":{}}});
            assert_eq!(
                physical_bind(&compose.config, Some(&model)).unwrap(),
                "/work"
            );
            let mut extra = model;
            extra["services"]["db"]["volumes"] =
                serde_json::json!([{"type":"volume","source":"data","target":"/data"}]);
            assert!(matches!(
                physical_bind(&compose.config, Some(&extra)),
                Err(Failure::ExtraMount)
            ));
        }

        #[cfg(unix)]
        #[test]
        fn approved_lifecycle_uses_managed_identity_shared_files_and_no_python() {
            let fixture = Fixture::new();
            let reviewed = fixture.config(r#"{"image":"existing:test","workspaceFolder":"/work","workspaceMount":"type=bind,source=${localWorkspaceFolder},target=/work","shutdownAction":"none","remoteEnv":{"VALUE":"test"},"onCreateCommand":["echo","created"],"updateContentCommand":["echo","updated"],"postCreateCommand":["echo","postcreated"],"postStartCommand":["echo","started"],"postAttachCommand":["echo","attached"]}"#);
            let id = "a".repeat(64);
            let inspect = serde_json::json!([{"Id":id,"State":{"Running":true},"Config":{"Env":[],"User":"1000"},"Mounts":[{"Type":"bind","Source":fixture.0,"Destination":"/work","RW":true}]}]);
            let inspection = fixture.0.join("inspect.json");
            std::fs::write(&inspection, serde_json::to_vec(&inspect).unwrap()).unwrap();
            let log = fixture.0.join("operations");
            let docker = fixture.executable("fake docker", &format!("printf '%s\\n' \"$*\" >> {}\ncase \"$1\" in\ncreate) printf '%s\\n' {};;\nstart) :;;\ncontainer) cat {};;\nexec) for last do :; done; case \"$last\" in /work/.solder-bind-*) cat \"${{last##*/}}\";; *) :;; esac;;\n*) exit 12;;\nesac", remote::quoted(log.to_str().unwrap()), id, remote::quoted(inspection.to_str().unwrap())));
            let mut attached = attach(
                reviewed,
                Arc::new(AtomicBool::new(false)),
                Some(docker.clone()),
            )
            .unwrap();
            attached.managed.activated();
            assert!(!attached.mount.is_mirror());
            assert_eq!(attached.mount.root, fixture.0);
            assert_eq!(
                attached.mount.remote_path(&fixture.0.join("file")).unwrap(),
                "/work/file"
            );
            std::fs::write(fixture.0.join("file"), "shared bytes").unwrap();
            assert_eq!(
                remote::read(&fixture.0.join("file")).unwrap(),
                b"shared bytes"
            );
            let Endpoint::Container {
                program,
                id: actual,
                env,
                ..
            } = &attached.mount.target.endpoint
            else {
                panic!("expected container");
            };
            assert_eq!(program, &docker);
            assert_eq!(actual, &id);
            assert_eq!(env, &vec![("VALUE".into(), "test".into())]);
            let operations = std::fs::read_to_string(log).unwrap();
            let lines: Vec<_> = operations.lines().collect();
            assert!(lines[0].starts_with("create --pull=never --name solder_"));
            assert!(lines[1].starts_with("start "));
            let hooks: Vec<_> = lines
                .iter()
                .filter(|line| line.starts_with("exec ") && line.contains(" echo "))
                .map(|line| line.split_whitespace().last().unwrap())
                .collect();
            assert_eq!(
                hooks,
                ["created", "updated", "postcreated", "started", "attached"]
            );
            assert!(!operations.contains("python"));
            drop(attached);
            assert!(remote::mount(&fixture.0).unwrap().is_none());
        }

        #[test]
        fn an_unshared_daemon_path_is_refused_and_the_probe_is_removed() {
            let fixture = Fixture::new();
            let reviewed = fixture.config(r#"{"image":"existing:test","workspaceFolder":"/work","workspaceMount":"type=bind,source=${localWorkspaceFolder},target=/work"}"#);
            let runtime = Runtime::new(reviewed.config, None, "solder_probe_test").unwrap();
            let id = ContainerId::parse(&"a".repeat(64)).unwrap();
            let inspect = serde_json::json!([{"Id":id.as_str(),"State":{"Running":true},"Config":{"Env":[],"User":""}}]);
            let container = runtime
                .container_from_inspect(&id, &serde_json::to_vec(&inspect).unwrap())
                .unwrap();
            let mut called = false;
            let mut runner = |plan: &CommandPlan| {
                called = true;
                assert_eq!(plan.operation, Operation::Exec);
                assert!(plan.args.iter().any(|arg| arg == "cat"));
                Ok(CommandOutput {
                    code: Some(0),
                    stdout: b"other project".to_vec(),
                    stderr: Vec::new(),
                })
            };
            assert!(matches!(
                verify_shared_files(&runtime, &container, &mut runner),
                Err(Failure::SharedFiles)
            ));
            assert!(called);
            assert!(
                std::fs::read_dir(&fixture.0)
                    .unwrap()
                    .flatten()
                    .all(|file| !file
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".solder-bind-"))
            );
        }

        #[cfg(unix)]
        #[test]
        fn changed_approval_never_invokes_a_runner() {
            let fixture = Fixture::new();
            let reviewed = fixture.config(r#"{"image":"existing:test"}"#);
            let log = fixture.0.join("invoked");
            let docker = fixture.executable(
                "fake docker",
                &format!("touch {}", remote::quoted(log.to_str().unwrap())),
            );
            std::fs::write(
                fixture.0.join(".devcontainer.json"),
                r#"{"image":"other:test"}"#,
            )
            .unwrap();
            assert!(matches!(
                attach(reviewed, Arc::new(AtomicBool::new(false)), Some(docker)),
                Err(Failure::Changed)
            ));
            assert!(!log.exists());
        }
    }
}

// Workspace integration
use crate::{
    container_ui::{self, Review, ReviewEvent},
    lang::{t, tf},
    picker::Picker,
    remote::{Mount, Target},
    remote_ui::{self, SshEvent, SshInput},
    workspace::Workspace,
};
use gpui::{Context, Window};
use std::{io, path::Path, sync::Arc};

pub(crate) fn own_cache_root(root: &Path) -> bool {
    is_cached_project(root)
}
pub(crate) fn is_cached_project(root: &Path) -> bool {
    execution::cached_under(&crate::settings::config_dir(), root)
}
pub(crate) fn prepare_cached(root: &Path) -> io::Result<Option<Arc<Mount>>> {
    execution::prepare_under(&crate::settings::config_dir(), root)
}
pub(crate) fn connect_cached(target: Target) -> io::Result<Arc<Mount>> {
    execution::connect_under(&crate::settings::config_dir(), target)
}

fn failure_text(error: execution::Failure) -> String {
    match error {
        execution::Failure::NoConfig => t("No devcontainer configuration found").to_owned(),
        execution::Failure::Changed => {
            t("Devcontainer configuration changed. Review it again.").to_owned()
        }
        execution::Failure::Bind => {
            t("Use one writable bind mount from the project root to workspaceFolder").to_owned()
        }
        execution::Failure::ExtraMount => t("Extra container mounts are not supported").to_owned(),
        execution::Failure::SharedFiles => {
            t("The container does not share the host project files").to_owned()
        }
        execution::Failure::Runtime(error) => tf("Dev container failed: {}", &[&error]),
        execution::Failure::Io(error) => tf("Dev container failed: {}", &[&error]),
    }
}

impl Workspace {
    /// Switch process environment without replacing a document, pane or tab.
    pub(crate) fn retain_remote(&mut self, mount: Arc<Mount>, cx: &mut Context<Self>) {
        let changed = self
            .remote_mount
            .as_ref()
            .is_none_or(|old| !old.is_connected() || !execution::same_environment(old, &mount));
        self.remote_status = None;
        self.remote_mount = Some(mount);
        if changed {
            self.remote_environment_changed(cx);
        }
        cx.notify();
    }

    pub(crate) fn connect_ssh(
        &mut self,
        _: &remote_ui::ConnectSsh,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = cx.weak_entity();
        self.toggle_modal(window, cx, move |window, cx| {
            cx.subscribe_in(
                &cx.entity(),
                window,
                move |_, _, event: &SshEvent, _, cx| {
                    let SshEvent::Accepted(target) = event;
                    workspace
                        .update(cx, |workspace, cx| workspace.start_ssh(target.clone(), cx))
                        .ok();
                },
            )
            .detach();
            Picker::new(SshInput::new(), window, cx)
        });
    }

    fn start_ssh(&mut self, target: Target, cx: &mut Context<Self>) {
        self.remote_status = Some(t("Connecting to SSH").into());
        let cancellation = execution::Cancellation::new();
        let cancel = cancellation.0.clone();
        self.remote_task = Some(cx.spawn(async move |this, cx| {
            let _cancellation = cancellation;
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mount = connect_cached(target)?;
                    if cancel.load(std::sync::atomic::Ordering::Acquire) {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "SSH connection cancelled",
                        ));
                    }
                    Ok(mount)
                })
                .await;
            match result {
                Ok(mount) => {
                    // Carry the strong mount through windows::open and into the
                    // destination before this completion releases its reference.
                    cx.update(|cx| {
                        let window = crate::windows::open(mount.root.clone(), cx);
                        window
                            .update(cx, |workspace, window, cx| {
                                workspace.retain_remote(mount, cx);
                                if workspace.session_writer.is_none() {
                                    workspace.start_session(window, cx);
                                }
                            })
                            .ok();
                    })
                    .ok();
                    this.update(cx, |this, cx| {
                        this.remote_status = None;
                        cx.notify();
                    })
                    .ok();
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.remote_status =
                            Some(tf("SSH connection failed: {}", &[&error]).into());
                        cx.notify();
                    })
                    .ok();
                }
            }
        }));
        cx.notify();
    }

    pub(crate) fn open_devcontainer(
        &mut self,
        _: &container_ui::OpenDevContainer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.remote_mount.is_some() {
            self.remote_status = Some(t("A remote environment is already active").into());
            cx.notify();
            return;
        }
        let root = self.root(cx);
        if own_cache_root(&root) {
            self.remote_status = Some(t("Open a local project to use a dev container").into());
            cx.notify();
            return;
        }
        self.remote_status = Some(t("Reading devcontainer configuration").into());
        self.remote_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { execution::Reviewed::load(&root) })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(reviewed) => this.review_devcontainer(reviewed, window, cx),
                Err(error) => {
                    this.remote_status = Some(failure_text(error).into());
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn review_devcontainer(
        &mut self,
        reviewed: execution::Reviewed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remote_status = None;
        let workspace = cx.weak_entity();
        let config = reviewed.config.clone();
        let details = reviewed.details.clone();
        let mut reviewed = Some(reviewed);
        self.toggle_modal(window, cx, move |window, cx| {
            cx.subscribe_in(
                &cx.entity(),
                window,
                move |_, _, event: &ReviewEvent, _, cx| {
                    if let ReviewEvent::Approved(config) = event
                        && let Some(mut reviewed) = reviewed.take()
                    {
                        reviewed.config = (**config).clone();
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.start_devcontainer(reviewed, cx)
                            })
                            .ok();
                    }
                },
            )
            .detach();
            Review::with_details(config, details, cx)
        });
    }

    fn start_devcontainer(&mut self, reviewed: execution::Reviewed, cx: &mut Context<Self>) {
        if self.remote_mount.is_some() {
            return;
        }
        self.remote_status = Some(t("Starting dev container").into());
        let cancellation = execution::Cancellation::new();
        let cancel = cancellation.0.clone();
        self.remote_task = Some(cx.spawn(async move |this, cx| {
            let _cancellation = cancellation;
            let result = cx
                .background_executor()
                .spawn(async move { execution::attach(reviewed, cancel, None) })
                .await;
            this.update(cx, |this, cx| match result {
                Ok(execution::Attached { mount, mut managed }) => {
                    this.retain_remote(mount, cx);
                    managed.activated();
                    cx.on_release(move |_, _| drop(managed)).detach();
                }
                Err(error) => {
                    this.remote_status = Some(failure_text(error).into());
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }
}
