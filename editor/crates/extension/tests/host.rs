//! The host against real extensions, in a world that answers from a script:
//! Zed's HTML extension for the whole path, and one extension for each shape
//! the API has had before it.

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
    /// What the user set for a server, by the name an extension asks with.
    settings: Vec<(&'static str, String)>,
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
        // The file each of the fixtures looks for once its package is there.
        for server in [SERVER, "vue-language-server.js"] {
            std::fs::write(package.join("bin").join(server), "").unwrap();
        }
        Ok(())
    }

    /// GitHub cannot be reached: an extension that needs a release says so.
    fn release(&self, repo: &str, tag: Option<&str>, pre: bool) -> Result<Release, String> {
        self.note(format!("release {repo} {tag:?} pre={pre}"));
        Err("GitHub is out of reach".into())
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

    fn settings(&self, category: &str, key: Option<&str>) -> Option<String> {
        self.note(format!("settings {category} {key:?}"));
        let (_, json) = self
            .settings
            .iter()
            .find(|(name, _)| category == "lsp" && key == Some(name))?;
        Some(json.clone())
    }
}

fn fixture(name: &str) -> extension::Extension {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    extension::manifest::read(&dir).unwrap()
}

fn html() -> extension::Extension {
    fixture("html")
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
        api: "0.8.0".into(),
    };
    assert_eq!(
        Host::load(&old, &work, world.clone()).err().unwrap(),
        "It was built for version 0.8.0 of Zed's extension API, which Solder does not run yet"
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

/// Loads a fixture in a world where the server is, or is not, on the PATH,
/// asks for its server, and gives the answer and what the extension did.
fn ask(name: &str, on_path: Option<&str>) -> (Result<Command, String>, Vec<String>, Host) {
    let extension = fixture(name);
    let work = work_dir(&format!("host-{name}-{}", on_path.is_some()));
    let world = Arc::new(Script {
        on_path: on_path.map(str::to_string),
        ..Default::default()
    });
    let host = Host::load(&extension, &work, world.clone()).unwrap();
    let project = work_dir(&format!("host-{name}-project"));
    let answer = host.language_server_command(&extension.servers[0].id, &project);
    (answer, world.take(), host)
}

fn built_for(name: &str) -> String {
    match fixture(name).code {
        extension::Code::Zed { api } => api,
        other => panic!("{name} has {other:?}"),
    }
}

#[test]
fn an_extension_for_the_first_api_is_asked_by_name_and_language() {
    // 0.0.1: every function belongs to the world itself, and the server is
    // described by a record. This one goes straight to GitHub.
    assert_eq!(built_for("pest"), "0.0.1");
    let (answer, did, host) = ask("pest", None);
    assert!(answer.unwrap_err().contains("GitHub is out of reach"));
    assert_eq!(
        did,
        [
            "pest: CheckingForUpdate",
            "release pest-parser/pest-ide-tools None pre=false",
        ]
    );
    // It has no settings to give, in a version that could not give any.
    let project = work_dir("host-pest-options");
    assert_eq!(host.initialization_options("pest", &project), Ok(None));
    assert_eq!(host.workspace_configuration("pest", &project), Ok(None));
}

#[test]
fn an_extension_for_api_0_0_6_is_asked_by_id() {
    assert_eq!(built_for("nginx"), "0.0.6");
    assert_eq!(fixture("nginx").servers[0].languages, ["Nginx"]);
    let (answer, did, host) = ask("nginx", Some("/usr/local/bin/nginx-language-server"));
    let command = answer.unwrap();
    assert_eq!(command.command, "/usr/local/bin/nginx-language-server");
    assert_eq!(did, ["which nginx-language-server"]);
    let project = work_dir("host-nginx-options");
    assert_eq!(host.initialization_options("nginx", &project), Ok(None));
    assert_eq!(host.workspace_configuration("nginx", &project), Ok(None));
    // Without the server, it says what to install in its own words.
    let (answer, _, _) = ask("nginx", None);
    assert!(answer.unwrap_err().contains("nginx-language-server"));
}

#[test]
fn an_extension_for_api_0_1_0_uses_the_path_then_github() {
    assert_eq!(built_for("terraform"), "0.1.0");
    let (answer, did, _) = ask("terraform", Some("/opt/bin/terraform-ls"));
    let command = answer.unwrap();
    assert_eq!(command.command, "/opt/bin/terraform-ls");
    assert_eq!(command.args, ["serve"]);
    assert_eq!(did, ["which terraform-ls"]);
    let (answer, did, _) = ask("terraform", None);
    assert!(answer.unwrap_err().contains("GitHub is out of reach"));
    assert_eq!(
        did,
        [
            "which terraform-ls",
            "terraform-ls: CheckingForUpdate",
            "release hashicorp/terraform-ls None pre=false",
        ]
    );
}

#[test]
fn an_extension_for_api_0_3_0_asks_for_a_pre_release() {
    assert_eq!(built_for("ledger"), "0.3.0");
    let (answer, did, _) = ask("ledger", None);
    assert!(answer.unwrap_err().contains("GitHub is out of reach"));
    assert_eq!(
        did,
        [
            "ledger-language-server: CheckingForUpdate",
            "release claytonrcarter/ledger-language-server None pre=true",
        ]
    );
}

#[test]
fn an_extension_adds_to_the_options_of_a_server_that_is_not_its_own() {
    // Vue's extension brings its own server and needs the TypeScript server
    // to load a plugin: from API 0.4 it can say so.
    let extension = fixture("vue");
    let work = work_dir("host-vue");
    let world = Arc::new(Script {
        latest: "3.0.0".into(),
        ..Default::default()
    });
    let host = Host::load(&extension, &work, world.clone()).unwrap();
    let project = work_dir("host-vue-project");
    let vue = "vue-language-server";

    // Its own server first: that installs the server and the plugin.
    let command = host.language_server_command(vue, &project).unwrap();
    assert_eq!(command.command, "/opt/node/bin/node");
    let script = work.join("node_modules/@vue/language-server/bin/vue-language-server.js");
    assert_eq!(command.args, [script.to_str().unwrap(), "--stdio"]);
    let did = world.take();
    for package in ["@vue/language-server", "@vue/typescript-plugin"] {
        assert!(did.contains(&format!("install {package}@3.0.0")), "{did:?}");
    }

    // For the TypeScript server it has a plugin, found in its own folder.
    let added = host
        .additional_initialization_options(vue, "typescript-language-server", &project)
        .unwrap()
        .expect("something for the TypeScript server");
    let added: serde_json::Value = serde_json::from_str(&added).unwrap();
    assert_eq!(added["plugins"][0]["name"], "@vue/typescript-plugin");
    assert_eq!(added["plugins"][0]["location"], work.to_str().unwrap());
    // For another one that speaks TypeScript it has settings instead.
    let settings = host
        .additional_workspace_configuration(vue, "vtsls", &project)
        .unwrap()
        .expect("settings for vtsls");
    let settings: serde_json::Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(
        settings["vtsls"]["tsserver"]["globalPlugins"][0]["name"],
        "@vue/typescript-plugin"
    );
    // And nothing for a server it has no business with.
    assert_eq!(
        host.additional_initialization_options(vue, "gopls", &project),
        Ok(None)
    );
    assert_eq!(
        host.additional_workspace_configuration(vue, "gopls", &project),
        Ok(None)
    );

    // An extension built before 0.4 cannot add anything, and is not asked.
    let (_, _, old) = ask("nginx", Some("/usr/local/bin/nginx-language-server"));
    assert_eq!(
        old.additional_initialization_options("nginx", "typescript-language-server", &project),
        Ok(None)
    );
}

#[test]
fn the_users_settings_reach_the_extension() {
    let project = work_dir("host-settings-project");
    let vue = "vue-language-server";
    let json =
        |text: Option<String>| serde_json::from_str::<serde_json::Value>(&text.unwrap()).unwrap();

    // Nothing set: Vue's extension gives its own defaults.
    let world = Arc::new(Script {
        latest: "3.0.0".into(),
        ..Default::default()
    });
    let host = Host::load(
        &fixture("vue"),
        &work_dir("host-settings-unset"),
        world.clone(),
    )
    .unwrap();
    assert_eq!(
        json(host.initialization_options(vue, &project).unwrap()),
        serde_json::json!({})
    );
    let defaults = json(host.workspace_configuration(vue, &project).unwrap());
    assert_eq!(defaults["vue.inlayHints.missingProps"], true);
    // It asks by a name of its own choosing, not by the server's id.
    assert!(
        world
            .take()
            .contains(&"settings lsp Some(\"vue\")".to_string())
    );

    // Set: what the user wrote is what the server will be sent.
    let options = serde_json::json!({ "typescript": { "tsdk": "/opt/typescript/lib" } });
    let settings = serde_json::json!({ "vue.inlayHints.missingProps": false });
    let world = Arc::new(Script {
        latest: "3.0.0".into(),
        settings: vec![(
            "vue",
            extension::world::lsp_settings(None, None, Some(&options), Some(&settings)),
        )],
        ..Default::default()
    });
    let host = Host::load(&fixture("vue"), &work_dir("host-settings-set"), world).unwrap();
    assert_eq!(
        json(host.initialization_options(vue, &project).unwrap()),
        options
    );
    assert_eq!(
        json(host.workspace_configuration(vue, &project).unwrap()),
        settings
    );
}
