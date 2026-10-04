//! What one extension may do outside its sandbox, and what it did.
//!
//! Installing an extension allows it everything its code does: run the
//! commands its manifest declares, install packages from npm, download
//! files. The user may take each of these back later, for that extension
//! alone, and a host it downloads from by name. A [`Gate`] stands between
//! the extension and the [`World`]: it refuses what was taken back and
//! writes down what was done and what was refused, so the Extensions tab
//! can say what works from what happened and not from the manifest alone.

use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::host::{Command, FileKind, HttpRequest, HttpResponse, Output, Release, Status, World};

/// What the user took back from an extension. Nothing, by default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Refusals {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub commands: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub npm: bool,
    /// Every download, whatever the host.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub downloads: bool,
    /// Hosts it may not download from while downloads are allowed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
}

/// Something an extension did, or was kept from doing, since the app
/// started. Each is kept once, however often it happened.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Event {
    /// A command, as it was run.
    Ran(String),
    /// A package from npm.
    Installed(String),
    /// A host it downloaded from.
    Downloaded(String),
    /// What it was refused, in words: `Download from example.com`.
    Refused(String),
    /// What it said went wrong with a server it was getting ready.
    Failed(String),
}

pub type Did = Arc<Mutex<BTreeSet<Event>>>;

pub struct Gate {
    world: Arc<dyn World>,
    refusals: Arc<Mutex<Refusals>>,
    did: Did,
}

/// A command as it would be typed.
fn line(command: &Command) -> String {
    std::iter::once(command.command.as_str())
        .chain(command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The host of an address: `github.com` of `https://github.com/a/b`.
pub fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    host.split(':').next().unwrap_or_default()
}

impl Gate {
    pub fn new(world: Arc<dyn World>, refusals: Arc<Mutex<Refusals>>, did: Did) -> Self {
        Self {
            world,
            refusals,
            did,
        }
    }

    fn note(&self, event: Event) {
        self.did
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(event);
    }

    /// Passes if `refused` says no; otherwise writes the refusal down and
    /// gives the extension the reason.
    fn allowed(&self, refused: impl FnOnce(&Refusals) -> bool, what: String) -> Result<(), String> {
        let no = refused(&self.refusals.lock().unwrap_or_else(|e| e.into_inner()));
        if no {
            self.note(Event::Refused(what.clone()));
            return Err(format!("{what}: refused for this extension"));
        }
        Ok(())
    }

    fn may_download(&self, url: &str) -> Result<String, String> {
        let host = host_of(url).to_string();
        self.allowed(
            |r| r.downloads || r.hosts.contains(&host),
            format!("Download from {host}"),
        )?;
        Ok(host)
    }

    fn may_use_npm(&self, package: &str) -> Result<(), String> {
        self.allowed(|r| r.npm, format!("Install {package} from npm"))
    }
}

impl World for Gate {
    fn node(&self) -> Result<String, String> {
        self.world.node()
    }

    fn npm_latest(&self, package: &str) -> Result<String, String> {
        self.may_use_npm(package)?;
        self.world.npm_latest(package)
    }

    fn npm_install(&self, dir: &Path, package: &str, version: &str) -> Result<(), String> {
        self.may_use_npm(package)?;
        self.world.npm_install(dir, package, version)?;
        self.note(Event::Installed(package.to_string()));
        Ok(())
    }

    fn release(&self, repo: &str, tag: Option<&str>, pre_release: bool) -> Result<Release, String> {
        // Looking is not downloading, but it is the same request to the
        // same place, and refusing the one without the other would only
        // move the failure.
        self.may_download("https://github.com")?;
        self.world.release(repo, tag, pre_release)
    }

    fn download(&self, url: &str, dest: &Path, kind: FileKind) -> Result<(), String> {
        let host = self.may_download(url)?;
        self.world.download(url, dest, kind)?;
        self.note(Event::Downloaded(host));
        Ok(())
    }

    fn fetch(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        let host = self.may_download(&request.url)?;
        let response = self.world.fetch(request)?;
        self.note(Event::Downloaded(host));
        Ok(response)
    }

    fn run(&self, command: &Command) -> Result<Output, String> {
        let line = line(command);
        self.allowed(|r| r.commands, format!("Run {line}"))?;
        let output = self.world.run(command)?;
        self.note(Event::Ran(line));
        Ok(output)
    }

    fn which(&self, binary: &str) -> Option<String> {
        self.world.which(binary)
    }

    fn env(&self) -> Vec<(String, String)> {
        self.world.env()
    }

    fn status(&self, server: &str, status: Status) {
        if let Status::Failed(why) = &status {
            self.note(Event::Failed(format!("{server}: {why}")));
        }
        self.world.status(server, status);
    }

    fn settings(&self, category: &str, key: Option<&str>) -> Option<String> {
        self.world.settings(category, key)
    }

    fn undeclared(&self, command: &Command) {
        self.note(Event::Refused(format!(
            "Run {}, which its manifest does not declare",
            line(command)
        )));
        self.world.undeclared(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A world where everything works, and says nothing.
    struct Open;

    impl World for Open {
        fn node(&self) -> Result<String, String> {
            Ok("node".into())
        }
        fn npm_latest(&self, _: &str) -> Result<String, String> {
            Ok("1.0.0".into())
        }
        fn npm_install(&self, _: &Path, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn release(&self, _: &str, _: Option<&str>, _: bool) -> Result<Release, String> {
            Ok(Release::default())
        }
        fn download(&self, _: &str, _: &Path, _: FileKind) -> Result<(), String> {
            Ok(())
        }
        fn fetch(&self, _: HttpRequest) -> Result<HttpResponse, String> {
            Err("no answer".into())
        }
        fn run(&self, _: &Command) -> Result<Output, String> {
            Ok(Output::default())
        }
        fn which(&self, _: &str) -> Option<String> {
            None
        }
        fn env(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn status(&self, _: &str, _: Status) {}
    }

    #[test]
    fn hosts_are_read_from_addresses() {
        assert_eq!(host_of("https://github.com/a/b"), "github.com");
        assert_eq!(
            host_of("https://user:pw@example.com:8443/x?y"),
            "example.com"
        );
        assert_eq!(host_of("http://127.0.0.1:9000"), "127.0.0.1");
        assert_eq!(host_of("example.org/file"), "example.org");
    }

    #[test]
    fn what_was_taken_back_is_refused_and_written_down() {
        let refusals = Arc::new(Mutex::new(Refusals::default()));
        let did = Did::default();
        let gate = Gate::new(Arc::new(Open), refusals.clone(), did.clone());
        let dest = Path::new("x");
        let command = Command {
            command: "gem".into(),
            args: vec!["install".into(), "solargraph".into()],
            env: Vec::new(),
        };

        // With nothing taken back, everything goes through and is noted
        // once, however often it happens.
        gate.download("https://github.com/a/b.zip", dest, FileKind::Zip)
            .unwrap();
        gate.download("https://github.com/a/c.zip", dest, FileKind::Zip)
            .unwrap();
        gate.npm_latest("typescript").unwrap();
        gate.npm_install(dest, "typescript", "5.0.0").unwrap();
        gate.run(&command).unwrap();
        gate.release("a/b", None, false).unwrap();
        // What failed in the world is not something that was done.
        gate.fetch(HttpRequest {
            method: "GET",
            url: "https://example.com/x".into(),
            headers: Vec::new(),
            body: None,
            redirects: None,
        })
        .unwrap_err();
        gate.status("demo-ls", Status::Failed("no release".into()));
        gate.status("demo-ls", Status::Downloading);
        let seen = |did: &Did| did.lock().unwrap().iter().cloned().collect::<Vec<_>>();
        assert_eq!(
            seen(&did),
            [
                Event::Ran("gem install solargraph".into()),
                Event::Installed("typescript".into()),
                Event::Downloaded("github.com".into()),
                Event::Failed("demo-ls: no release".into()),
            ]
        );

        // One host taken back: the others still answer.
        did.lock().unwrap().clear();
        refusals.lock().unwrap().hosts = vec!["github.com".into()];
        let refused = gate
            .download("https://github.com/a/b.zip", dest, FileKind::Zip)
            .unwrap_err();
        assert_eq!(
            refused,
            "Download from github.com: refused for this extension"
        );
        assert!(gate.release("a/b", None, false).is_err());
        gate.download("https://example.com/b.zip", dest, FileKind::Zip)
            .unwrap();
        // Everything taken back.
        *refusals.lock().unwrap() = Refusals {
            commands: true,
            npm: true,
            downloads: true,
            hosts: Vec::new(),
        };
        assert!(
            gate.download("https://example.com/b.zip", dest, FileKind::Zip)
                .is_err()
        );
        assert!(gate.npm_latest("typescript").is_err());
        assert!(gate.npm_install(dest, "typescript", "5.0.0").is_err());
        assert!(gate.run(&command).is_err());
        // A command the manifest does not declare never reaches the world;
        // the host says so, and that is written down too.
        gate.undeclared(&command);
        // Looking for a program and reading the environment are not
        // things to take back.
        assert_eq!(gate.node(), Ok("node".into()));
        assert_eq!(
            seen(&did),
            [
                Event::Downloaded("example.com".into()),
                Event::Refused("Download from example.com".into()),
                Event::Refused("Download from github.com".into()),
                Event::Refused("Install typescript from npm".into()),
                Event::Refused("Run gem install solargraph".into()),
                Event::Refused(
                    "Run gem install solargraph, which its manifest does not declare".into()
                ),
            ]
        );
    }

    #[test]
    fn refusals_are_written_only_where_there_are_any() {
        assert_eq!(serde_json::to_string(&Refusals::default()).unwrap(), "{}");
        let some = Refusals {
            npm: true,
            hosts: vec!["example.com".into()],
            ..Default::default()
        };
        let text = serde_json::to_string(&some).unwrap();
        assert_eq!(text, r#"{"npm":true,"hosts":["example.com"]}"#);
        assert_eq!(serde_json::from_str::<Refusals>(&text).unwrap(), some);
    }
}
