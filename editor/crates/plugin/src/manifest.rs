//! `plugin.json`: who the plugin is, what it may do and what it adds.

use std::path::Path;

use serde::Deserialize;
use solder_plugin::{Event, Request};

/// The file next to `plugin.wasm`.
pub const MANIFEST: &str = "plugin.json";
pub const MODULE: &str = "plugin.wasm";

/// Something a plugin may do once the user has agreed to it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    StatusBar,
    EditorRead,
    EditorWrite,
    FsRead,
    /// Requests to this host, on any port.
    Http(String),
}

impl Permission {
    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "statusBar" => Self::StatusBar,
            "editor:read" => Self::EditorRead,
            "editor:write" => Self::EditorWrite,
            "fs:read" => Self::FsRead,
            _ => match name.strip_prefix("http:") {
                Some(host) if valid_host(host) => Self::Http(host.to_ascii_lowercase()),
                Some(_) => return Err(format!("\"{name}\" does not name a host")),
                None => return Err(format!("Unknown permission \"{name}\"")),
            },
        })
    }

    /// As written in the manifest.
    pub fn name(&self) -> String {
        match self {
            Self::StatusBar => "statusBar".into(),
            Self::EditorRead => "editor:read".into(),
            Self::EditorWrite => "editor:write".into(),
            Self::FsRead => "fs:read".into(),
            Self::Http(host) => format!("http:{host}"),
        }
    }

    /// What the user agrees to.
    pub fn describe(&self) -> String {
        match self {
            Self::StatusBar => "Show text in the status bar".into(),
            Self::EditorRead => "Read the file in front and its selection".into(),
            Self::EditorWrite => "Change the file in front".into(),
            Self::FsRead => "Read files of the project (not .env or files kept from AI)".into(),
            Self::Http(host) => format!("Send requests to {host}"),
        }
    }
}

/// One name or address: no scheme, path, port, user or wildcard.
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

/// The host of an http(s) URL, lowercased, without user, port or path.
pub fn url_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // What follows the last `@` is the host: `http://ok.com@evil.com/`.
    let host_port = authority.rsplit('@').next()?;
    let host = match host_port.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => host_port,
    };
    valid_host(host).then(|| host.to_ascii_lowercase())
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Command {
    pub id: String,
    /// Shown in the command palette.
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub permissions: Vec<Permission>,
    /// The events it wants: `activate`, `open`, `save`, `change`.
    pub events: Vec<String>,
    pub commands: Vec<Command>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Raw {
            name: String,
            #[serde(default)]
            version: String,
            #[serde(default)]
            description: String,
            #[serde(default)]
            permissions: Vec<String>,
            #[serde(default)]
            events: Vec<String>,
            #[serde(default)]
            commands: Vec<Command>,
        }
        let raw: Raw =
            serde_json::from_str(text).map_err(|e| format!("{MANIFEST} is not valid: {e}"))?;
        // The name is a folder and a key in settings.
        if raw.name.is_empty()
            || raw.name.len() > 64
            || !raw
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err("A plugin's name is lowercase letters, digits and dashes".into());
        }
        let mut permissions = raw
            .permissions
            .iter()
            .map(|p| Permission::parse(p))
            .collect::<Result<Vec<_>, _>>()?;
        permissions.sort();
        permissions.dedup();
        for event in &raw.events {
            if !matches!(event.as_str(), "activate" | "open" | "save" | "change") {
                return Err(format!("Unknown event \"{event}\""));
            }
        }
        Ok(Self {
            name: raw.name,
            version: raw.version,
            description: raw.description,
            permissions,
            events: raw.events,
            commands: raw.commands,
        })
    }

    /// Reads `plugin.json` in `dir`; blocking.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(dir.join(MANIFEST))
            .map_err(|e| format!("Cannot read {MANIFEST}: {e}"))?;
        Self::parse(&text)
    }

    /// Whether the plugin asked to hear about `event`. Its own commands
    /// always reach it.
    pub fn wants(&self, event: &Event) -> bool {
        match event {
            Event::Command { id } => self.commands.iter().any(|c| &c.id == id),
            other => self.events.iter().any(|e| e == other.kind()),
        }
    }

    /// Whether a declared permission covers `request`; the reason if not.
    pub fn allows(&self, request: &Request) -> Result<(), String> {
        let needed = match request {
            Request::Log { .. } => return Ok(()),
            Request::Status { .. } => Permission::StatusBar,
            Request::EditorText => Permission::EditorRead,
            Request::Edit { .. } => Permission::EditorWrite,
            Request::ReadFile { .. } => Permission::FsRead,
            Request::Http(http) => match url_host(&http.url) {
                Some(host) => Permission::Http(host),
                None => return Err("Only http and https URLs with a host name".into()),
            },
        };
        if self.permissions.contains(&needed) {
            Ok(())
        } else {
            Err(format!(
                "The plugin did not declare the permission {}",
                needed.name()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solder_plugin::HttpRequest;

    #[test]
    fn reads_a_manifest_and_rejects_what_it_does_not_know() {
        let manifest = Manifest::parse(
            r#"{"name": "request-timer", "version": "1.0.0",
                "permissions": ["statusBar", "http:API.github.com", "statusBar"],
                "events": ["open"],
                "commands": [{"id": "run", "title": "Timer: Run"}]}"#,
        )
        .unwrap();
        assert_eq!(
            manifest.permissions,
            [
                Permission::StatusBar,
                Permission::Http("api.github.com".into())
            ]
        );
        assert_eq!(manifest.permissions[1].name(), "http:api.github.com");
        assert!(manifest.wants(&Event::Command { id: "run".into() }));
        assert!(!manifest.wants(&Event::Command { id: "other".into() }));
        assert!(manifest.wants(&Event::Open {
            path: "a".into(),
            language: None
        }));
        assert!(!manifest.wants(&Event::Save { path: "a".into() }));

        for (text, why) in [
            (r#"{"name": "Bad Name"}"#, "lowercase"),
            (
                r#"{"name": "a", "permissions": ["fs:write"]}"#,
                "Unknown permission",
            ),
            (
                r#"{"name": "a", "permissions": ["http:*"]}"#,
                "does not name a host",
            ),
            (
                r#"{"name": "a", "permissions": ["http:a.com/x"]}"#,
                "does not name a host",
            ),
            (r#"{"name": "a", "events": ["keypress"]}"#, "Unknown event"),
            (r#"{"name": "../up"}"#, "lowercase"),
        ] {
            let error = Manifest::parse(text).unwrap_err();
            assert!(error.contains(why), "{text}: {error}");
        }
    }

    #[test]
    fn requests_need_the_declared_permission() {
        let manifest = Manifest::parse(
            r#"{"name": "a", "permissions": ["editor:read", "http:api.example.com"]}"#,
        )
        .unwrap();
        let get = |url: &str| {
            Request::Http(HttpRequest {
                method: "GET".into(),
                url: url.into(),
                ..Default::default()
            })
        };
        assert!(manifest.allows(&Request::EditorText).is_ok());
        assert!(manifest.allows(&Request::Log { text: "x".into() }).is_ok());
        assert!(
            manifest
                .allows(&get("https://api.example.com/v1?q=1"))
                .is_ok()
        );
        assert!(
            manifest
                .allows(&get("http://API.example.com:8080/"))
                .is_ok()
        );
        for refused in [
            Request::Status { text: "x".into() },
            Request::ReadFile { path: "a".into() },
            Request::Edit {
                path: "a".into(),
                start: 0,
                end: 0,
                text: "".into(),
            },
            get("https://example.com/"),
            get("https://evil.com/api.example.com"),
            // The host is what follows the `@`.
            get("https://api.example.com@evil.com/"),
            get("https://api.example.com.evil.com/"),
            get("file:///etc/passwd"),
            get("https:///nohost"),
        ] {
            assert!(manifest.allows(&refused).is_err(), "{refused:?}");
        }
        assert_eq!(
            url_host("https://user:pw@Host.dev:443/x"),
            Some("host.dev".into())
        );
    }
}
