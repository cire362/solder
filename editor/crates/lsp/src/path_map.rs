use std::{collections::HashSet, path::Path, str::FromStr};

use lsp_types::Uri;
use serde_json::Value;
use url::Url;

use crate::{Error, Result, path_to_uri, uri_to_path};

/// A local project cache and its absolute POSIX root on a remote machine.
/// This is lexical mapping, with no filesystem access or symlink resolution.
/// Unknown schemes and files outside either root stay unchanged. Callers
/// must check navigation and edits against the local project separately.
#[derive(Clone, Debug)]
pub struct PathMap {
    local: Root,
    remote: Root,
}

#[derive(Clone, Debug)]
struct Root {
    url: Url,
    segments: Vec<String>,
}

#[derive(Clone, Copy)]
enum Direction {
    Remote,
    Local,
}

impl PathMap {
    pub fn new(local_root: impl AsRef<Path>, remote_root: impl AsRef<str>) -> Result<Self> {
        let local_root = local_root.as_ref();
        if !local_root.is_absolute() {
            return Err(Error::Parse("path map needs an absolute local root".into()));
        }
        let local = Url::parse(path_to_uri(local_root).as_str())
            .ok()
            .and_then(Root::new)
            .ok_or_else(|| Error::Parse("invalid local path map root".into()))?;
        let remote = posix_url(remote_root.as_ref())
            .and_then(Root::new)
            .ok_or_else(|| Error::Parse("path map needs an absolute POSIX remote root".into()))?;
        Ok(Self { local, remote })
    }

    pub fn to_remote_uri(&self, uri: &Uri) -> Uri {
        self.map_uri(uri.as_str(), Direction::Remote)
            .and_then(|uri| Uri::from_str(&uri).ok())
            .unwrap_or_else(|| uri.clone())
    }

    pub fn to_local_uri(&self, uri: &Uri) -> Uri {
        self.map_uri(uri.as_str(), Direction::Local)
            .and_then(|uri| Uri::from_str(&uri).ok())
            .unwrap_or_else(|| uri.clone())
    }

    /// Maps an LSP payload's URI fields and workspace-edit keys. Ordinary
    /// strings and opaque data, command arguments and options are preserved.
    pub fn to_remote(&self, value: &mut Value) {
        self.map_value(value, Direction::Remote);
    }

    pub fn to_local(&self, value: &mut Value) {
        self.map_value(value, Direction::Local);
    }

    pub(crate) fn outgoing_message(&self, message: &mut Value) {
        self.map_message(message, Direction::Remote);
    }

    pub(crate) fn incoming_message(&self, message: &mut Value) {
        self.map_message(message, Direction::Local);
    }

    fn map_message(&self, message: &mut Value, direction: Direction) {
        // IDs and methods can themselves look like URIs; they are not paths.
        for key in ["params", "result"] {
            if let Some(value) = message.get_mut(key) {
                self.map_value(value, direction);
            }
        }
    }

    fn map_uri(&self, uri: &str, direction: Direction) -> Option<String> {
        let url = Url::parse(uri).ok()?;
        if url.scheme() != "file" {
            return None;
        }
        let (from, to) = match direction {
            Direction::Remote => (&self.local, &self.remote),
            Direction::Local => (&self.remote, &self.local),
        };
        if url.host_str() != from.url.host_str() {
            return None;
        }
        let mut segments = segments(&url)?;
        let trailing_slash = url.path().ends_with('/');
        while segments.last().is_some_and(String::is_empty) {
            segments.pop();
        }
        let suffix = segments.strip_prefix(from.segments.as_slice())?;
        let mut mapped = to.url.clone();
        {
            let mut path = mapped.path_segments_mut().ok()?;
            path.pop_if_empty().extend(suffix);
            if trailing_slash && !suffix.is_empty() {
                path.push("");
            }
        }
        mapped.set_query(url.query());
        mapped.set_fragment(url.fragment());
        Some(mapped.into())
    }

    fn map_path(&self, path: &str, direction: Direction) -> Option<String> {
        match direction {
            Direction::Remote => {
                let path = Path::new(path);
                if !path.is_absolute() {
                    return None;
                }
                let mapped = self.map_uri(path_to_uri(path).as_str(), direction)?;
                let url = Url::parse(&mapped).ok()?;
                Some(format!("/{}", segments(&url)?.join("/")))
            }
            Direction::Local => {
                let url = posix_url(path)?;
                let mapped = self.map_uri(url.as_str(), direction)?;
                uri_to_path(&Uri::from_str(&mapped).ok()?)?
                    .to_str()
                    .map(str::to_owned)
            }
        }
    }

    fn map_value(&self, value: &mut Value, direction: Direction) {
        match value {
            Value::Array(values) => {
                for value in values {
                    self.map_value(value, direction);
                }
            }
            Value::Object(fields) => {
                for (key, value) in fields {
                    match key.as_str() {
                        // These have server-defined schemas, including source
                        // text and shell commands; their strings are not paths.
                        "data" | "arguments" | "initializationOptions" | "experimental" => {}
                        "uri" | "rootUri" | "targetUri" | "oldUri" | "newUri" | "scopeUri"
                        | "baseUri" | "target" | "href" => {
                            if let Some(mapped) =
                                value.as_str().and_then(|uri| self.map_uri(uri, direction))
                            {
                                *value = Value::String(mapped);
                            } else if !value.is_string() {
                                self.map_value(value, direction);
                            }
                        }
                        "rootPath" => {
                            if let Some(mapped) = value
                                .as_str()
                                .and_then(|path| self.map_path(path, direction))
                            {
                                *value = Value::String(mapped);
                            }
                        }
                        "changes" | "relatedDocuments" => self.map_uri_keys(value, direction),
                        _ => self.map_value(value, direction),
                    }
                }
            }
            _ => {}
        }
    }

    fn map_uri_keys(&self, value: &mut Value, direction: Direction) {
        let Value::Object(entries) = value else {
            self.map_value(value, direction);
            return;
        };
        let keys: Vec<_> = entries
            .keys()
            .map(|key| self.map_uri(key, direction).unwrap_or_else(|| key.clone()))
            .collect();
        for value in entries.values_mut() {
            self.map_value(value, direction);
        }
        // Aliases or a mixture of local and remote keys must not silently
        // overwrite edits. Leave an ambiguous map for the caller to reject.
        if keys.iter().collect::<HashSet<_>>().len() == keys.len() {
            *entries = keys
                .into_iter()
                .zip(std::mem::take(entries).into_values())
                .collect();
        }
    }
}

impl Root {
    fn new(mut url: Url) -> Option<Self> {
        let mut segments = segments(&url)?;
        while segments.last().is_some_and(String::is_empty) {
            segments.pop();
        }
        url.path_segments_mut().ok()?.clear().extend(&segments);
        Some(Self { url, segments })
    }
}

// POSIX paths must not go through host-native Path/Url conversion: on
// Windows a remote "/work" has no drive and is not a local absolute path.
fn posix_url(path: &str) -> Option<Url> {
    if !path.starts_with('/') || path.contains('\0') {
        return None;
    }
    let parts: Vec<_> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.iter().any(|part| matches!(*part, "." | "..")) {
        return None;
    }
    let mut url = Url::parse("file:///").ok()?;
    url.path_segments_mut().ok()?.extend(parts);
    Some(url)
}

fn segments(url: &Url) -> Option<Vec<String>> {
    url.path_segments()?.map(decode_segment).collect()
}

fn decode_segment(segment: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(segment.len());
    let mut input = segment.bytes();
    while let Some(byte) = input.next() {
        let byte = if byte == b'%' {
            let high = (input.next()? as char).to_digit(16)?;
            let low = (input.next()? as char).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        };
        // Encoded separators cannot be allowed to become path components
        // when the app converts the translated URI with its native helper.
        if byte == 0 || byte == b'/' || (cfg!(windows) && byte == b'\\') {
            return None;
        }
        bytes.push(byte);
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn local_root() -> std::path::PathBuf {
        std::env::temp_dir().join("solder path map 日本語 %")
    }

    #[test]
    fn project_paths_round_trip_for_posix_roots_and_encoded_names() {
        let local = local_root();
        let names = ["plain", "a b", "日本語", "русский", "100%", "#?+", "%2F"];
        for remote in ["/", "/work/project", "/work/Проект % #/"] {
            let map = PathMap::new(&local, remote).unwrap();
            for first in names {
                for second in names {
                    for path in [local.clone(), local.join(first).join(second)] {
                        let uri = path_to_uri(&path);
                        let there = map.to_remote_uri(&uri);
                        assert_ne!(there, uri);
                        assert_eq!(map.to_local_uri(&there), uri);
                        assert_eq!(uri_to_path(&map.to_local_uri(&there)).unwrap(), path);
                    }
                }
            }
        }
    }

    #[test]
    fn roots_and_prefix_boundaries_are_exact() {
        let local = local_root();
        let map = PathMap::new(&local, "/work/project/").unwrap();
        assert_eq!(
            map.to_remote_uri(&path_to_uri(&local)).as_str(),
            "file:///work/project"
        );
        assert_eq!(
            map.to_local_uri(&Uri::from_str("file:///work/project").unwrap()),
            path_to_uri(&local)
        );
        for uri in [
            "file:///work/project-other/a",
            "file:///work/project/../elsewhere/a",
            "file:///work/project/%2e%2e/elsewhere/a",
            "file:///work/elsewhere/a",
            "file://other/work/project/a",
            "untitled:/work/project/a",
            "https://example.com/work/project/a",
            "file:///work/project/a%2Fb",
            "file:///work/project/%00",
            "file:///work/project/%ff",
        ] {
            let uri = Uri::from_str(uri).unwrap();
            assert_eq!(map.to_local_uri(&uri), uri);
        }
        let outside = path_to_uri(
            &local
                .with_file_name("solder path map 日本語 %-other")
                .join("a"),
        );
        assert_eq!(map.to_remote_uri(&outside), outside);
        let mut malformed = json!({"uri": "file:///work/project/a%xy"});
        let original = malformed.clone();
        map.to_local(&mut malformed);
        assert_eq!(malformed, original);
    }

    #[test]
    fn escaped_roots_and_windows_uri_segments_are_matched_in_uri_space() {
        let map = PathMap {
            local: Root::new(Url::parse("file:///C:/cache%20dir").unwrap()).unwrap(),
            remote: Root::new(posix_url("/work/root").unwrap()).unwrap(),
        };
        let uri = Uri::from_str("file:///C%3a/c%61che%20dir/%E6%97%A5%20%25.rs").unwrap();
        let remote = map.to_remote_uri(&uri);
        assert_eq!(remote.as_str(), "file:///work/root/%E6%97%A5%20%25.rs");
        let local = map.to_local_uri(&remote);
        assert_eq!(local.as_str(), "file:///C:/cache%20dir/%E6%97%A5%20%25.rs");
    }

    #[test]
    fn invalid_roots_are_refused() {
        assert!(PathMap::new("relative", "/work").is_err());
        for remote in [
            "relative",
            "C:\\work",
            "/work/../other",
            "/work/./other",
            "/work\0",
        ] {
            assert!(PathMap::new(local_root(), remote).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_local_filesystem_root_can_map_to_a_remote_subdirectory() {
        let map = PathMap::new("/", "/work/project").unwrap();
        let uri = path_to_uri(Path::new("/src/日本語.rs"));
        assert_eq!(
            map.to_remote_uri(&uri).as_str(),
            "file:///work/project/src/%E6%97%A5%E6%9C%AC%E8%AA%9E.rs"
        );
        assert_eq!(map.to_local_uri(&map.to_remote_uri(&uri)), uri);
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_paths_and_unc_roots_use_the_existing_helpers() {
        for root in ["C:\\cache dir", "\\\\host\\share\\cache dir"] {
            let map = PathMap::new(root, "/work").unwrap();
            let path = Path::new(root).join("日本語.rs");
            let uri = path_to_uri(&path);
            assert_eq!(map.to_local_uri(&map.to_remote_uri(&uri)), uri);
            let mut payload = json!({"rootPath": root});
            map.to_remote(&mut payload);
            assert_eq!(payload["rootPath"], "/work");
            map.to_local(&mut payload);
            assert_eq!(payload["rootPath"], root);
        }
    }

    #[test]
    fn uri_fields_and_edit_keys_map_without_changing_source_labels_or_commands() {
        let local = local_root();
        let map = PathMap::new(&local, "/work/project").unwrap();
        let uri = path_to_uri(&local.join("a b.rs")).as_str().to_owned();
        let mut payload = json!({
            "rootUri": path_to_uri(&local), "rootPath": local.to_str().unwrap(),
            "workspaceFolders": [{"uri": uri, "name": uri}],
            "textDocument": {"uri": uri, "text": uri},
            "contentChanges": [{"text": uri}],
            "location": {"uri": uri}, "locations": [{"targetUri": uri}],
            "diagnostics": [{"message": uri, "relatedInformation": [{"location": {"uri": uri}}], "codeDescription": {"href": uri}}],
            "links": [{"target": uri, "tooltip": uri}],
            "hint": {"label": [{"value": uri, "location": {"uri": uri}}]},
            "edit": {"changes": {&uri: [{"newText": uri}]}, "documentChanges": [
                {"textDocument": {"uri": uri}, "edits": [{"newText": uri}]},
                {"kind": "rename", "oldUri": uri, "newUri": uri},
                {"kind": "create", "uri": uri}, {"kind": "delete", "uri": uri}
            ]},
            "relatedDocuments": {&uri: {"items": []}},
            "scopeUri": uri, "globPattern": {"baseUri": uri},
            "label": uri, "title": uri, "command": uri,
            "commands": [{"command": uri, "arguments": [uri, {"uri": uri}]}],
            "data": {"uri": uri}, "initializationOptions": {"uri": uri}, "experimental": {"uri": uri}
        });
        let original = payload.clone();
        map.to_remote(&mut payload);
        let remote_uri = "file:///work/project/a%20b.rs";
        assert_eq!(payload["rootPath"], "/work/project");
        for pointer in [
            "/workspaceFolders/0/uri",
            "/textDocument/uri",
            "/location/uri",
            "/locations/0/targetUri",
            "/diagnostics/0/relatedInformation/0/location/uri",
            "/diagnostics/0/codeDescription/href",
            "/links/0/target",
            "/hint/label/0/location/uri",
            "/edit/documentChanges/0/textDocument/uri",
            "/edit/documentChanges/1/oldUri",
            "/edit/documentChanges/1/newUri",
            "/edit/documentChanges/2/uri",
            "/edit/documentChanges/3/uri",
            "/scopeUri",
            "/globPattern/baseUri",
        ] {
            assert_eq!(payload.pointer(pointer).unwrap(), remote_uri, "{pointer}");
        }
        assert_eq!(
            payload["edit"]["changes"][remote_uri],
            json!([{"newText": uri}])
        );
        assert!(payload["relatedDocuments"].get(remote_uri).is_some());
        for pointer in [
            "/textDocument/text",
            "/contentChanges/0/text",
            "/workspaceFolders/0/name",
            "/diagnostics/0/message",
            "/links/0/tooltip",
            "/hint/label/0/value",
            "/label",
            "/title",
            "/command",
            "/commands/0/command",
            "/commands/0/arguments/0",
            "/commands/0/arguments/1/uri",
            "/data/uri",
            "/initializationOptions/uri",
            "/experimental/uri",
        ] {
            assert_eq!(payload.pointer(pointer).unwrap(), &uri, "{pointer}");
        }
        map.to_local(&mut payload);
        assert_eq!(payload, original);
    }

    #[test]
    fn aliases_do_not_overwrite_workspace_edits() {
        let map = PathMap::new(local_root(), "/work").unwrap();
        let mut payload = json!({"changes": {
            "file:///work/a": [{"newText": "one"}],
            "file:///work/%61": [{"newText": "two"}]
        }});
        let original = payload.clone();
        map.to_local(&mut payload);
        assert_eq!(payload, original);
    }

    #[test]
    fn message_envelopes_and_uri_suffixes_are_preserved() {
        let map = PathMap::new(local_root(), "/work").unwrap();
        let mut message = json!({"id": "file:///work/a", "method": "file:///work/a",
            "params": {"uri": "file:///work/a?query#fragment"},
            "result": [{"uri": "file:///work/b"}],
            "error": {"message": "file:///work/a", "data": {"uri": "file:///work/a"}}
        });
        let original = message.clone();
        map.incoming_message(&mut message);
        assert_ne!(message["params"], original["params"]);
        assert_ne!(message["result"], original["result"]);
        map.outgoing_message(&mut message);
        assert_eq!(message, original);
    }
}
