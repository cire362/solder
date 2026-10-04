//! The host against a real extension: Zed's HTML extension, in a world
//! that answers from a script.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use extension::host::{
    Command, FileKind, Host, HttpRequest, HttpResponse, Output, Release, Status, World,
};

const PACKAGE: &str = "@zed-industries/vscode-langservers-extracted";
const SERVER: &str = "vscode-html-language-server";

/// What the extension did, in order, and what it finds when it looks.
#[derive(Default)]
struct Script {
    log: Mutex<Vec<String>>,
    /// Where `which` finds the server, if it is on the user's PATH.
    on_path: Option<String>,
    latest: String,
}

impl Script {
    fn note(&self, line: String) {
        self.log.lock().unwrap().push(line);
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

impl World for Script {
    fn node(&self) -> Result<String, String> {
        Ok("/opt/node/bin/node".into())
    }

    fn npm_latest(&self, package: &str) -> Result<String, String> {
        self.note(format!("latest {package}"));
        Ok(self.latest.clone())
    }

    /// What npm does, as far as the extension can tell: the package's
    /// manifest and its server appear under `node_modules`.
    fn npm_install(&self, dir: &Path, package: &str, version: &str) -> Result<(), String> {
        self.note(format!("install {package}@{version}"));
        let package = dir.join("node_modules").join(package);
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::write(
            package.join("package.json"),
            format!(r#"{{"version": "{version}"}}"#),
        )
        .unwrap();
        std::fs::write(package.join("bin").join(SERVER), "").unwrap();
        Ok(())
    }

    fn release(&self, repo: &str, _: Option<&str>, _: bool) -> Result<Release, String> {
        Err(format!("unexpected release of {repo}"))
    }

    fn download(&self, url: &str, _: &Path, _: FileKind) -> Result<(), String> {
        Err(format!("unexpected download of {url}"))
    }

    fn fetch(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        Err(format!("unexpected fetch of {}", request.url))
    }

    fn run(&self, command: &Command) -> Result<Output, String> {
        Err(format!("unexpected run of {}", command.command))
    }

    fn which(&self, binary: &str) -> Option<String> {
        self.note(format!("which {binary}"));
        self.on_path.clone()
    }

    fn env(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn status(&self, server: &str, status: Status) {
        self.note(format!("{server}: {status:?}"));
    }
}

fn html() -> extension::Extension {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/html");
    extension::manifest::read(&dir).unwrap()
}

fn work_dir(name: &str) -> PathBuf {
    let dir = extension::testing::scratch(name);
    dir.canonicalize().unwrap()
}

#[test]
fn an_extension_installs_its_server_and_says_how_to_start_it() {
    let extension = html();
    assert_eq!(extension.servers[0].id, SERVER);
    let work = work_dir("host-install");
    let world = Arc::new(Script {
        latest: "4.10.0".into(),
        ..Default::default()
    });
    let host = Host::load(&extension, &work, world.clone()).unwrap();
    let project = work_dir("host-install-project");

    // Nothing is installed: it asks npm for the newest version, installs
    // it, and answers with Node and the server's path in its own folder.
    let command = host.language_server_command(SERVER, &project).unwrap();
    let server = work
        .join("node_modules")
        .join(PACKAGE)
        .join("bin")
        .join(SERVER);
    assert_eq!(command.command, "/opt/node/bin/node");
    assert_eq!(command.args, [server.to_str().unwrap(), "--stdio"]);
    assert_eq!(
        world.take(),
        [
            format!("which {SERVER}"),
            format!("{SERVER}: CheckingForUpdate"),
            format!("latest {PACKAGE}"),
            format!("{SERVER}: Downloading"),
            format!("install {PACKAGE}@4.10.0"),
        ]
    );

    // Asked again, it finds the version it installed and installs nothing.
    let again = host.language_server_command(SERVER, &project).unwrap();
    assert_eq!(again, command);
    assert!(!world.take().iter().any(|line| line.starts_with("install")));

    assert_eq!(
        host.initialization_options(SERVER, &project)
            .unwrap()
            .as_deref(),
        Some(r#"{"provideFormatter":true}"#)
    );
    assert_eq!(
        host.workspace_configuration(SERVER, &project).unwrap(),
        None
    );
}

#[test]
fn a_server_on_the_path_is_used_as_it_is() {
    let work = work_dir("host-path");
    let world = Arc::new(Script {
        on_path: Some("/usr/local/bin/vscode-html-language-server".into()),
        ..Default::default()
    });
    let host = Host::load(&html(), &work, world.clone()).unwrap();
    let command = host
        .language_server_command(SERVER, &work_dir("host-path-project"))
        .unwrap();
    assert_eq!(
        command.command,
        "/usr/local/bin/vscode-html-language-server"
    );
    assert_eq!(command.args, ["--stdio"]);
    assert_eq!(world.take(), [format!("which {SERVER}")]);
    // Nothing was written to its folder.
    assert!(std::fs::read_dir(&work).unwrap().next().is_none());
}

#[test]
fn what_cannot_be_run_says_why() {
    let work = work_dir("host-refuse");
    let world = Arc::new(Script::default());
    let mut old = html();
    old.code = extension::Code::Zed {
        api: "0.1.0".into(),
    };
    assert_eq!(
        Host::load(&old, &work, world.clone()).err().unwrap(),
        "It was built for version 0.1.0 of Zed's extension API, which Solder does not run yet"
    );
    let mut plain = html();
    plain.code = extension::Code::None;
    assert_eq!(
        Host::load(&plain, &work, world.clone()).err().unwrap(),
        "The extension has no code to run"
    );
    // A world that fails is reported in the extension's own words.
    struct NoNpm(Script);
    impl World for NoNpm {
        fn node(&self) -> Result<String, String> {
            self.0.node()
        }
        fn npm_latest(&self, _: &str) -> Result<String, String> {
            Err("npm was not found".into())
        }
        fn npm_install(&self, dir: &Path, package: &str, version: &str) -> Result<(), String> {
            self.0.npm_install(dir, package, version)
        }
        fn release(&self, repo: &str, tag: Option<&str>, pre: bool) -> Result<Release, String> {
            self.0.release(repo, tag, pre)
        }
        fn download(&self, url: &str, dest: &Path, kind: FileKind) -> Result<(), String> {
            self.0.download(url, dest, kind)
        }
        fn fetch(&self, request: HttpRequest) -> Result<HttpResponse, String> {
            self.0.fetch(request)
        }
        fn run(&self, command: &Command) -> Result<Output, String> {
            self.0.run(command)
        }
        fn which(&self, _: &str) -> Option<String> {
            None
        }
        fn env(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn status(&self, _: &str, _: Status) {}
    }
    let host = Host::load(&html(), &work, Arc::new(NoNpm(Script::default()))).unwrap();
    let error = host
        .language_server_command(SERVER, &work_dir("host-refuse-project"))
        .unwrap_err();
    assert!(error.contains("npm was not found"), "{error}");
}
