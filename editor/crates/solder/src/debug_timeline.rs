//! The request timeline of a debug run: the HTTP requests a Node program
//! serves and sends and the SQL it runs, each with the line that made it.
//! `timeline.js` records them from inside the program (loaded with
//! `--require`) as JSON lines in a file this module reads back.

use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use serde_json::Value;

const PRELOAD: &str = include_str!("timeline.js");

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A request the program served.
    In,
    /// A request the program sent.
    Out,
    Sql,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct Entry {
    pub kind: Kind,
    /// `GET /users`, `GET https://api.example.com/x`, or the query.
    pub label: String,
    /// Wall clock, in milliseconds.
    pub at: f64,
    pub ms: f64,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub rows: Option<u64>,
    /// A served request's id, which the work done for it names as `parent`.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub line: Option<u32>,
}

impl Entry {
    pub fn failed(&self) -> bool {
        self.error.is_some() || self.status.is_some_and(|s| s >= 400)
    }
}

/// Writes the preload and an empty log for this editor process; blocking.
pub fn prepare(data_dir: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    let dir = data_dir.join("timeline");
    std::fs::create_dir_all(&dir)?;
    let preload = dir.join("timeline.js");
    if std::fs::read_to_string(&preload).ok().as_deref() != Some(PRELOAD) {
        std::fs::write(&preload, PRELOAD)?;
    }
    let log = dir.join(format!("run-{}.jsonl", std::process::id()));
    std::fs::write(&log, "")?;
    Ok((preload, log))
}

/// Loads the preload into a Node launch request and its child processes.
pub fn add_env(request: &mut Value, preload: &Path, log: &Path) {
    if request["type"] != "pwa-node" {
        return;
    }
    // Quoted: the data directory may hold a space ("Application Support").
    let require = format!("--require \"{}\"", preload.display());
    let options = match std::env::var("NODE_OPTIONS") {
        Ok(existing) if !existing.trim().is_empty() => format!("{existing} {require}"),
        _ => require,
    };
    if !request["env"].is_object() {
        request["env"] = serde_json::json!({});
    }
    request["env"]["NODE_OPTIONS"] = options.into();
    request["env"]["SOLDER_TIMELINE"] = log.display().to_string().into();
}

/// What was appended to `path` since `offset`; blocking.
pub fn read_from(path: &Path, offset: u64) -> (Vec<u8>, u64) {
    let mut out = Vec::new();
    let Ok(mut file) = std::fs::File::open(path) else {
        return (out, offset);
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return (out, offset);
    }
    let read = file.read_to_end(&mut out).unwrap_or(0);
    (out, offset + read as u64)
}

/// Complete lines from `pending`, leaving a partial last line there.
pub fn take_entries(pending: &mut Vec<u8>) -> Vec<Entry> {
    let Some(end) = pending.iter().rposition(|b| *b == b'\n') else {
        return Vec::new();
    };
    let complete: Vec<u8> = pending.drain(..=end).collect();
    complete
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

/// Lines are written as work finishes; shown in the order it began, with
/// the work done for a served request right under it.
pub fn order(entries: &mut [Entry]) {
    let started: HashMap<&str, f64> = entries
        .iter()
        .filter_map(|e| Some((e.id.as_deref()?, e.at)))
        .collect();
    let mut keyed: Vec<((f64, bool, f64), Entry)> = entries
        .iter()
        .map(|e| {
            let group = e.parent.as_deref().and_then(|p| started.get(p)).copied();
            ((group.unwrap_or(e.at), group.is_some(), e.at), e.clone())
        })
        .collect();
    keyed.sort_by(|(a, _), (b, _)| {
        a.0.total_cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.total_cmp(&b.2))
    });
    for (slot, (_, e)) in entries.iter_mut().zip(keyed) {
        *slot = e;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lines_as_they_complete() {
        let mut pending = br#"{"kind":"sql","label":"select 1","at":1,"ms":2.5,"rows":1,"path":"/p/db.js","line":4,"parent":"7:1"}
{"kind":"in","id":"7:1","label":"GET /","at":0,"ms":9,"status":500}
{"kind":"out","label":"GET http"#
            .to_vec();
        let entries = take_entries(&mut pending);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, Kind::Sql);
        assert_eq!(entries[0].line, Some(4));
        assert!(entries[1].failed());
        assert!(pending.starts_with(br#"{"kind":"out""#));
        pending.extend_from_slice(br#"://x/","at":2,"ms":1,"error":"refused"}"#);
        assert!(take_entries(&mut pending).is_empty());
        pending.push(b'\n');
        let out = take_entries(&mut pending);
        assert_eq!(out[0].error.as_deref(), Some("refused"));
        assert!(pending.is_empty());
    }

    #[test]
    fn adds_the_preload_to_node_runs_only() {
        let mut node = serde_json::json!({ "type": "pwa-node", "env": {} });
        add_env(
            &mut node,
            Path::new("/a b/timeline.js"),
            Path::new("/a b/run.jsonl"),
        );
        assert!(
            node["env"]["NODE_OPTIONS"]
                .as_str()
                .unwrap()
                .ends_with(r#"--require "/a b/timeline.js""#)
        );
        assert_eq!(node["env"]["SOLDER_TIMELINE"], "/a b/run.jsonl");
        let mut browser = serde_json::json!({ "type": "pwa-chrome" });
        add_env(&mut browser, Path::new("/t.js"), Path::new("/r"));
        assert!(browser.get("env").is_none());
    }

    #[test]
    fn puts_work_under_the_request_it_was_done_for() {
        let entry = |label: &str, at: f64, id: Option<&str>, parent: Option<&str>| Entry {
            kind: if id.is_some() { Kind::In } else { Kind::Sql },
            label: label.into(),
            at,
            ms: 1.,
            status: None,
            error: None,
            rows: None,
            id: id.map(Into::into),
            parent: parent.map(Into::into),
            path: None,
            line: None,
        };
        // As written: work finishes before the request that made it.
        let mut entries = vec![
            entry("query a", 2.0, None, Some("1")),
            entry("query b", 3.5, None, Some("2")),
            entry("GET /b", 3.0, Some("2"), None),
            entry("query c", 4.0, None, Some("1")),
            entry("GET /a", 1.0, Some("1"), None),
        ];
        order(&mut entries);
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            ["GET /a", "query a", "query c", "GET /b", "query b"]
        );
    }

    /// The preload in a real Node program, against stand-ins for the pg and
    /// mysql2 drivers.
    #[test]
    fn records_a_node_program() {
        let Some(node) = crate::lsp_store::find_program("node", Path::new("/")) else {
            eprintln!("node is not installed; skipped");
            return;
        };
        let dir = db::testing::dir("timeline-node").canonicalize().unwrap();
        for (path, text) in [
            (
                "node_modules/pg/index.js",
                "class Client { query() { return Promise.resolve({ rowCount: 2 }); } }\n\
                 module.exports = { Client };\n",
            ),
            (
                "node_modules/mysql2/index.js",
                "class Connection { query(sql, cb) { setImmediate(() => cb(null, [1, 2, 3])); } }\n\
                 module.exports = { Connection };\n",
            ),
            (
                "app.js",
                "const http = require('http');\n\
                 const { Client } = require('pg');\n\
                 const mysql = require('mysql2');\n\
                 const server = http.createServer(async (req, res) => {\n\
                 \x20 await new Client().query('select *\\n  from users');\n\
                 \x20 new mysql.Connection().query('select 1', () => res.end('ok'));\n\
                 });\n\
                 server.listen(0, '127.0.0.1', () => {\n\
                 \x20 const { port } = server.address();\n\
                 \x20 const url = `http://127.0.0.1:${port}/users?id=1`;\n\
                 \x20 http.get(url, { agent: false }, (r) => {\n\
                 \x20   r.resume();\n\
                 \x20   r.on('end', () => server.close());\n\
                 \x20 });\n\
                 });\n",
            ),
        ] {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let (preload, log) = prepare(&dir.join("data")).unwrap();
        let mut request = serde_json::json!({ "type": "pwa-node" });
        add_env(&mut request, &preload, &log);
        let mut node = std::process::Command::new(node)
            .arg("app.js")
            .current_dir(&dir)
            .env(
                "NODE_OPTIONS",
                request["env"]["NODE_OPTIONS"].as_str().unwrap(),
            )
            .env("SOLDER_TIMELINE", &log)
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // A program that does not exit fails the test instead of hanging it.
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(status) = node.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > std::time::Duration::from_secs(30) {
                let _ = node.kill();
                panic!("node did not exit");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(status.success());
        let mut entries = take_entries(&mut read_from(&log, 0).0);
        order(&mut entries);
        let seen: Vec<(Kind, &str, Option<u32>)> = entries
            .iter()
            .map(|e| (e.kind, e.label.as_str(), e.line))
            .collect();
        assert_eq!(
            seen,
            [
                (
                    Kind::Out,
                    &*format!("GET {}", seen[0].1.trim_start_matches("GET ")),
                    Some(11)
                ),
                (Kind::In, "GET /users?id=1", None),
                (Kind::Sql, "select * from users", Some(5)),
                (Kind::Sql, "select 1", Some(6)),
            ]
        );
        assert!(seen[0].1.starts_with("GET http://127.0.0.1:"));
        assert_eq!(entries[1].status, Some(200));
        assert_eq!((entries[2].rows, entries[3].rows), (Some(2), Some(3)));
        assert_eq!(entries[2].parent, entries[1].id);
        assert_eq!(
            entries[0].path.as_deref(),
            Some(dir.join("app.js").as_path())
        );
    }
}
