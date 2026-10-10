//! The check of a release: the most installed extensions of Open VSX, each
//! installed, started and asked for what it says it does, and the list of
//! what works written down.
//!
//! It runs the code of extensions, which has no sandbox. So it is a
//! program of its own for a machine that is thrown away afterwards (a CI
//! runner), not something the editor does: each extension gets a folder of
//! its own under a temporary root, a made-up project to look at, and a
//! limit of time. Nothing here is asked of the user, and what an extension
//! asks of the editor (a list to pick from, a file to show) is answered
//! with "nothing".

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    Entry, Extension, Origin, catalog,
    install::{self, Progress},
    vscode::{self, Told, VsHost},
};

/// Waits for something the catalog or an install does on their own
/// threads.
fn block<T>(future: impl Future<Output = T>) -> Result<T, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    Ok(runtime.block_on(future))
}

/// What became of one extension.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub id: String,
    pub version: String,
    pub downloads: u64,
    /// What Solder takes from it without running anything, in a line.
    pub data: String,
    /// What it has that is known not to run, from its manifest.
    pub not_running: Vec<String>,
    pub code: Code,
}

/// How the code of an extension did.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Code {
    /// It has none: it is all data.
    #[default]
    None,
    /// It could not be installed, or its process did not start.
    Failed(String),
    /// Its start threw, or did not return in time.
    NotStarted(String),
    Started(Started),
}

/// What an extension did once its code was started.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Started {
    pub commands: usize,
    /// The languages it registered something for, `*` for every one.
    pub languages: Vec<String>,
    /// What it can do for them, as it told the editor.
    pub features: Vec<String>,
    /// What it was asked for a sample file, and whether it answered.
    pub asked: Vec<(String, Result<(), String>)>,
    pub views: usize,
    /// The parts of VS Code's API it asked for that Solder does not have.
    pub missing: Vec<String>,
    /// The first thing it logged as an error, if it logged one.
    pub error: Option<String>,
}

/// How the check is run.
pub struct Plan {
    /// Open VSX, or what stands in for it.
    pub catalog: String,
    /// How many of the most installed to try.
    pub count: usize,
    /// Exact ids and versions; empty selects the most installed instead.
    pub extensions: Vec<(String, String)>,
    /// Where they are installed: a folder that is thrown away.
    pub root: PathBuf,
    pub node: String,
    /// How long an extension's start may take.
    pub patience: Duration,
}

/// Runs the check; blocking, for as long as `count` starts take.
pub fn run(plan: &Plan, mut said: impl FnMut(&str)) -> Result<Vec<Row>, String> {
    let entries = if plan.extensions.is_empty() {
        block(catalog::search(Origin::VsCode, &plan.catalog, ""))??
    } else {
        let mut entries = Vec::new();
        for (id, version) in &plan.extensions {
            entries.push(block(catalog::version(&plan.catalog, id, version))??);
        }
        entries
    };
    let script = vscode::host_script(&plan.root.join("host"))?;
    let mut rows = Vec::new();
    for entry in entries.into_iter().take(plan.count) {
        said(&format!("{} {}", entry.id, entry.version));
        rows.push(one(plan, &script, entry));
    }
    Ok(rows)
}

fn one(plan: &Plan, script: &Path, entry: Entry) -> Row {
    let mut row = Row {
        id: entry.id.clone(),
        version: entry.version.clone(),
        downloads: entry.downloads,
        ..Default::default()
    };
    let installing = install::install(entry, plan.root.clone(), Progress::new());
    let extension = match block(installing).and_then(|installed| installed) {
        Ok(extension) => extension,
        Err(error) => {
            row.code = Code::Failed(error);
            return row;
        }
    };
    row.data = extension.provides();
    row.not_running = extension.missing.clone();
    if extension.node().is_some() {
        row.code = start(plan, script, &extension);
    }
    row
}

/// A file of each language the extension waits for, in a project made up
/// for it: what it is given to look at.
fn samples(extension: &Extension, project: &Path) -> Vec<(String, PathBuf)> {
    let endings = [
        ("javascript", "js"),
        ("typescript", "ts"),
        ("python", "py"),
        ("rust", "rs"),
        ("go", "go"),
        ("json", "json"),
        ("css", "css"),
        ("html", "html"),
        ("markdown", "md"),
        ("yaml", "yaml"),
        ("java", "java"),
        ("c", "c"),
        ("cpp", "cpp"),
        ("shellscript", "sh"),
        ("plaintext", "txt"),
    ];
    let waits = extension.node().map(|(_, wakes)| wakes).unwrap_or_default();
    let mut languages: Vec<String> = waits
        .iter()
        .filter_map(|event| event.strip_prefix("onLanguage:"))
        .map(str::to_string)
        .collect();
    // One that waits for no language is shown a plain file.
    languages.push("plaintext".into());
    languages.dedup();
    let mut files = Vec::new();
    for language in languages.into_iter().take(4) {
        // Its own language's ending, or the one everybody knows.
        let own = extension.languages.iter().find(|known| {
            known
                .aliases
                .first()
                .is_some_and(|id| id.eq_ignore_ascii_case(&language))
        });
        let ending = own
            .and_then(|known| known.suffixes.first().cloned())
            .or_else(|| {
                let known = endings.iter().find(|(id, _)| *id == language);
                known.map(|(_, ending)| ending.to_string())
            });
        let Some(ending) = ending else {
            continue;
        };
        let file = project.join(format!("sample.{ending}"));
        if std::fs::write(&file, "sample\n").is_ok() {
            files.push((language, file));
        }
    }
    files
}

fn start(plan: &Plan, script: &Path, extension: &Extension) -> Code {
    let folder = extension
        .dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let work = install::work_dir(&plan.root, &folder);
    let project = work.join("project");
    if let Err(error) = std::fs::create_dir_all(&project) {
        return Code::Failed(error.to_string());
    }
    let files = samples(extension, &project);
    let (tx, heard) = mpsc::channel::<Told>();
    let host = VsHost::start(
        &plan.node,
        script,
        &extension.dir,
        &work,
        &[],
        move |told| {
            let _ = tx.send(told);
        },
    );
    let host = match host {
        Ok(host) => Arc::new(host),
        Err(error) => return Code::Failed(error),
    };
    // What it asks of the editor is answered with nothing, at once: there
    // is nobody to pick from a list. The rest is kept to be read.
    let kept = Arc::new(Mutex::new(Vec::<Told>::new()));
    {
        let (host, kept) = (Arc::downgrade(&host), kept.clone());
        std::thread::spawn(move || {
            for told in heard {
                match told {
                    Told::Asked { id, .. } => {
                        let Some(host) = host.upgrade() else { break };
                        host.answer(id, Ok(Value::Null));
                    }
                    other => kept.lock().unwrap_or_else(|e| e.into_inner()).push(other),
                }
            }
        });
    }
    let uri = |file: &Path| format!("file://{}", file.display());
    let documents: Vec<Value> = files
        .iter()
        .map(|(language, file)| {
            json!({ "uri": uri(file), "languageId": language, "version": 1, "text": "sample\n" })
        })
        .collect();
    host.notify(
        "init",
        json!({
            "folders": [project],
            "documents": documents,
            "active": files.first().map(|(_, file)| json!({ "uri": uri(file) })),
        }),
    );
    if let Err(error) = host.request("activate", json!({}), plan.patience) {
        host.stop();
        return Code::NotStarted(line(&error));
    }
    // What it registers a moment after its start returned counts too.
    std::thread::sleep(Duration::from_millis(1500));
    let mut started = Started::default();
    let read = |started: &mut Started| {
        let kept = kept.lock().unwrap_or_else(|e| e.into_inner());
        started.commands = 0;
        started.views = 0;
        for told in kept.iter() {
            match told {
                Told::Command { registered, .. } => started.commands += usize::from(*registered),
                Told::Missing(name) if !started.missing.contains(name) => {
                    started.missing.push(name.clone())
                }
                Told::Log { level, text } if level == "error" && started.error.is_none() => {
                    started.error = Some(line(text))
                }
                Told::Said { method, params } if method == "providers" => {
                    let languages = params["languages"].as_array().into_iter().flatten();
                    started.languages = languages
                        .filter_map(|language| language.as_str().map(str::to_string))
                        .collect();
                }
                Told::Said { method, params } if method == "view" && params["gone"] != true => {
                    started.views += 1
                }
                _ => {}
            }
        }
    };
    read(&mut started);
    // Its main feature: what it says it can do for a language is asked of
    // it, for a sample file of that language, the way the editor asks.
    if !started.languages.is_empty() {
        let ask = |id: u64, method: &str, params: Value| -> Result<Value, String> {
            let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
            host.notify("lsp", json!({ "message": message }));
            let until = Instant::now() + plan.patience;
            loop {
                let answer = kept
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .find_map(|told| match told {
                        Told::Said { method, params }
                            if method == "lsp" && params["message"]["id"] == id =>
                        {
                            Some(params["message"].clone())
                        }
                        _ => None,
                    });
                if let Some(answer) = answer {
                    return match answer["error"]["message"].as_str() {
                        Some(error) => Err(line(error)),
                        None => Ok(answer["result"].clone()),
                    };
                }
                if Instant::now() > until {
                    return Err("no answer".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let can = ask(1, "initialize", json!({}))
            .map(|answer| answer["capabilities"].clone())
            .unwrap_or_default();
        let named = [
            (
                "completionProvider",
                "completions",
                "textDocument/completion",
            ),
            ("hoverProvider", "hovers", "textDocument/hover"),
            (
                "definitionProvider",
                "definitions",
                "textDocument/definition",
            ),
            (
                "documentFormattingProvider",
                "formatting",
                "textDocument/formatting",
            ),
            (
                "documentSymbolProvider",
                "symbols",
                "textDocument/documentSymbol",
            ),
            ("codeActionProvider", "code actions", ""),
            ("referencesProvider", "references", ""),
            ("renameProvider", "rename", ""),
            ("signatureHelpProvider", "signature help", ""),
            ("inlayHintProvider", "inlay hints", ""),
            ("semanticTokensProvider", "semantic colors", ""),
        ];
        // The file of a language it registered for, or any if for all.
        let sample = files
            .iter()
            .find(|(language, _)| started.languages.contains(language))
            .or(files.first());
        let mut number = 1;
        for (key, name, method) in named {
            if can[key].is_null() {
                continue;
            }
            started.features.push(name.to_string());
            let (Some((_, file)), false) = (sample, method.is_empty()) else {
                continue;
            };
            number += 1;
            let params = json!({
                "textDocument": { "uri": uri(file) },
                "position": { "line": 0, "character": 3 },
                "options": { "tabSize": 4, "insertSpaces": true },
            });
            started
                .asked
                .push((name.to_string(), ask(number, method, params).map(|_| ())));
        }
        read(&mut started);
    }
    // Opening and saving exercise a serializer without claiming a kernel
    // works, or running code from a notebook we downloaded.
    let file = project.join("sample.ipynb");
    for book in extension.notebooks.iter().filter(|book| book.opens(&file)) {
        let sample = json!({"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[
            {"cell_type":"code","id":"sample","source":"1 + 1","metadata":{},"execution_count":null,"outputs":[]}
        ]});
        let opened = std::fs::write(&file, sample.to_string())
            .map_err(|e| e.to_string())
            .and_then(|_| {
                host.request(
                    "notebook.open",
                    json!({"type":book.view_type,"uri":uri(&file)}),
                    plan.patience,
                )
            })
            .and_then(|answer| match answer["cells"].as_array() {
                Some(cells) if cells.len() == 1 && cells[0]["value"] == "1 + 1" => Ok(()),
                _ => Err("The sample cell was not read back".into()),
            });
        let read_ok = opened.is_ok();
        started.asked.push((
            format!("read .ipynb ({})", book.view_type),
            opened.map_err(|e| line(&e)),
        ));
        if read_ok {
            // Saving an unchanged cell would also pass if the serializer
            // ignored edits and merely returned the original file.
            let edited = "2 + 2\n";
            host.notify(
                "notebook.cell",
                json!({"uri":uri(&file),"handle":0,"value":edited}),
            );
            let saved = host
                .request("notebook.save", json!({"uri":uri(&file)}), plan.patience)
                .and_then(|answer| {
                    if answer == true {
                        Ok(())
                    } else {
                        Err("No save confirmation".into())
                    }
                })
                .and_then(|_| std::fs::read(&file).map_err(|e| e.to_string()))
                .and_then(|bytes| {
                    serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string())
                })
                .and_then(|value| {
                    let source = &value["cells"][0]["source"];
                    let text = source.as_str().map(str::to_string).or_else(|| {
                        source
                            .as_array()
                            .map(|lines| lines.iter().filter_map(Value::as_str).collect::<String>())
                    });
                    if value["nbformat"] == 4 && text.as_deref() == Some(edited) {
                        Ok(())
                    } else {
                        Err("The saved file lost the edited sample cell".into())
                    }
                });
            started.asked.push((
                format!("save .ipynb ({})", book.view_type),
                saved.map_err(|e| line(&e)),
            ));
        }
        host.notify("notebook.close", json!({"uri":uri(&file)}));
    }
    read(&mut started);
    host.stop();
    Code::Started(started)
}

/// One line of what an extension said, short enough for a table.
fn line(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default().trim();
    let mut short: String = first.chars().take(160).collect();
    if first.chars().count() > 160 {
        short.push('\u{2026}');
    }
    short
}

/// The list of what works, as a page to read.
pub fn report(rows: &[Row]) -> String {
    let cell = |text: &str| text.replace('|', "\\|").replace(['\n', '\r'], " ");
    let started = |row: &&Row| matches!(row.code, Code::Started(_));
    let with_code = rows.iter().filter(|row| row.code != Code::None).count();
    let mut out = String::from("# Extensions of Open VSX in Solder\n\n");
    out.push_str(&format!(
        "The {} selected extensions, each installed and started. {} are data alone; of the {} with code, {} started.\n\n",
        rows.len(),
        rows.len() - with_code,
        with_code,
        rows.iter().filter(started).count(),
    ));
    out.push_str("| Extension | Installs | Solder uses | Its code | Asked for, not here |\n|---|---|---|---|---|\n");
    for row in rows {
        let (code, missing) = match &row.code {
            Code::None => ("none".to_string(), String::new()),
            Code::Failed(error) => (
                format!("not installed or not started: {error}"),
                String::new(),
            ),
            Code::NotStarted(error) => (format!("its start failed: {error}"), String::new()),
            Code::Started(started) => {
                let mut parts = vec!["started".to_string()];
                if started.commands > 0 {
                    parts.push(format!("{} commands", started.commands));
                }
                if !started.features.is_empty() {
                    let of = started.languages.join(", ");
                    parts.push(format!("{} for {of}", started.features.join(", ")));
                }
                for (name, answer) in &started.asked {
                    parts.push(match answer {
                        Ok(()) => format!("{name} answered"),
                        Err(error) => format!("{name} failed ({error})"),
                    });
                }
                if started.views > 0 {
                    parts.push(format!("{} views", started.views));
                }
                if let Some(error) = &started.error {
                    parts.push(format!("logged an error: {error}"));
                }
                (parts.join("; "), started.missing.join(", "))
            }
        };
        let uses = match row.not_running.is_empty() {
            true => row.data.clone(),
            false => format!("{} (not: {})", row.data, row.not_running.join("; ")),
        };
        out.push_str(&format!(
            "| {} {} | {} | {} | {} | {} |\n",
            cell(&row.id),
            cell(&row.version),
            row.downloads,
            cell(&uses),
            cell(&code),
            cell(&missing),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Served, scratch, serve, vscode_extension, write, zip};

    #[test]
    fn a_pinned_notebook_extension_reads_and_saves_the_sample() {
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else { return };
        let dir = scratch("census-notebook");
        let extension = dir.join("book/extension");
        write(
            &extension.join("package.json"),
            r#"{
            "name":"book","publisher":"Test","version":"1.0.0","main":"main.js",
            "contributes":{"notebooks":[
              {"type":"test-book","selector":[{"filenamePattern":"*.ipynb"}]},
              {"type":"stale-book","selector":[{"filenamePattern":"*.ipynb"}]}
            ]}
        }"#,
        );
        write(
            &extension.join("main.js"),
            r#"
const v = require('vscode');
exports.activate = () => ['test-book', 'stale-book'].forEach(type => v.workspace.registerNotebookSerializer(type, {
  deserializeNotebook(bytes) {
    const file = JSON.parse(Buffer.from(bytes).toString());
    return new v.NotebookData(file.cells.map(cell => new v.NotebookCellData(v.NotebookCellKind.Code, cell.source, 'python')));
  },
  serializeNotebook(data) {
    return Buffer.from(JSON.stringify({nbformat:4,cells:data.cells.map(cell => ({source:type === 'stale-book' ? '1 + 1' : cell.value}))}));
  }
}));
"#,
        );
        let Some(vsix) = zip(&dir.join("book"), "extension") else {
            return;
        };
        let (files, _) = serve(vec![("/book.vsix", Served::ok(vsix))]);
        let metadata = json!({"namespace":"Test","name":"book","version":"1.0.0","files":{"download":format!("{files}/book.vsix")}});
        let wrong = json!({"namespace":"Test","name":"book","version":"2.0.0","files":{"download":format!("{files}/book.vsix")}});
        let (catalog, requests) = serve(vec![
            (
                "/api/Test/book/1.0.0",
                Served::ok(metadata.to_string().into_bytes()),
            ),
            (
                "/api/Test/book/0.0.0",
                Served::ok(wrong.to_string().into_bytes()),
            ),
        ]);
        assert!(
            block(catalog::version(&catalog, "Test.book", "0.0.0"))
                .unwrap()
                .is_err()
        );
        assert!(
            block(catalog::version(&catalog, "Test.book", "../1.0.0"))
                .unwrap()
                .is_err()
        );
        let rows = run(
            &Plan {
                catalog,
                count: 1,
                extensions: vec![("Test.book".into(), "1.0.0".into())],
                root: dir.join("root"),
                node: node.to_string_lossy().into_owned(),
                patience: Duration::from_secs(10),
            },
            |_| {},
        )
        .unwrap();
        let Code::Started(started) = &rows[0].code else {
            panic!("{:?}", rows[0].code)
        };
        assert_eq!(
            started.asked,
            [
                ("read .ipynb (test-book)".into(), Ok(())),
                ("save .ipynb (test-book)".into(), Ok(())),
                ("read .ipynb (stale-book)".into(), Ok(())),
                (
                    "save .ipynb (stale-book)".into(),
                    Err("The saved file lost the edited sample cell".into()),
                ),
            ]
        );
        assert_eq!(rows[0].version, "1.0.0");
        assert!(
            !requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.contains("search"))
        );
    }

    #[test]
    fn the_most_installed_are_installed_started_and_asked_what_they_do() {
        let node = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("node"))
                .find(|node| node.is_file())
        });
        let Some(node) = node else {
            eprintln!("skipped: no node to run an extension with");
            return;
        };
        let dir = scratch("census");
        // Three extensions as a catalog has them: the fixture, which has
        // commands, data and asks for something Solder lacks; one with
        // language features, of which one answers and one throws; and one
        // whose start throws.
        vscode_extension(&dir.join("demo/extension"));
        let coded = |name: &str, wakes: &str, code: &str| {
            let manifest = format!(
                r#"{{ "name": "{name}", "publisher": "Acme", "version": "1.0.0", "main": "main.js", "activationEvents": ["{wakes}"] }}"#
            );
            write(
                &dir.join(format!("{name}/extension/package.json")),
                &manifest,
            );
            write(&dir.join(format!("{name}/extension/main.js")), code);
        };
        coded(
            "lang",
            "onLanguage:python",
            r#"const vscode = require('vscode');
exports.activate = () => {
  vscode.languages.registerHoverProvider('python', { provideHover: () => new vscode.Hover('a word') });
  vscode.languages.registerDocumentFormattingEditProvider('python', {
    provideDocumentFormattingEdits() { throw new Error('no formatter found\nlook elsewhere'); },
  });
  vscode.window.showQuickPick(['a', 'b']).then((picked) => console.log('picked ' + picked));
};"#,
        );
        coded(
            "broken",
            "*",
            "exports.activate = () => { throw new Error('no license'); };",
        );
        let mut routes = Vec::new();
        let mut listed = Vec::new();
        for (name, downloads) in [("demo", 900), ("lang", 500), ("broken", 100)] {
            let Some(vsix) = zip(&dir.join(name), "extension") else {
                eprintln!("skipped: no python3 to build a .vsix");
                return;
            };
            let path: &'static str = Box::leak(format!("/{name}.vsix").into_boxed_str());
            routes.push((path, Served::ok(vsix)));
            listed.push((name, downloads, path));
        }
        let (files, _) = serve(routes);
        let listed: Vec<String> = listed
            .iter()
            .map(|(name, downloads, path)| {
                format!(
                    r#"{{"namespace":"Acme","name":"{name}","version":"1.0.0","downloadCount":{downloads},"files":{{"download":"{files}{path}"}}}}"#
                )
            })
            .collect();
        let search = format!(r#"{{"extensions":[{}]}}"#, listed.join(","));
        let (catalog, _) = serve(vec![("/api/-/search", Served::ok(search.into_bytes()))]);
        let plan = Plan {
            catalog,
            count: 3,
            extensions: Vec::new(),
            root: dir.join("root"),
            node: node.to_string_lossy().into_owned(),
            patience: Duration::from_secs(20),
        };
        let mut tried = Vec::new();
        let rows = run(&plan, |name| tried.push(name.to_string())).unwrap();
        assert_eq!(tried.len(), 3);
        assert_eq!(rows.len(), 3);

        // The fixture: its data, what of it does not run, and its code:
        // started, with its commands and what it asked for.
        assert_eq!(rows[0].id, "Acme.demo");
        assert_eq!(rows[0].downloads, 900);
        assert!(rows[0].data.contains("2 themes") && rows[0].data.contains("3 commands"));
        assert!(
            rows[0]
                .not_running
                .iter()
                .any(|what| what.contains("icon theme"))
        );
        let Code::Started(demo) = &rows[0].code else {
            panic!("{:?}", rows[0].code);
        };
        assert_eq!(demo.commands, 3);
        assert_eq!(demo.missing, ["comments.createCommentController"]);
        assert!(demo.languages.is_empty() && demo.asked.is_empty());

        // The one with language features: what it can do, and what it
        // answered when asked for a file of its language. What it asked
        // the user was answered with nothing, and did not hold it up.
        let Code::Started(lang) = &rows[1].code else {
            panic!("{:?}", rows[1].code);
        };
        assert_eq!(lang.languages, ["python"]);
        assert_eq!(lang.features, ["hovers", "formatting"]);
        assert_eq!(
            lang.asked,
            [
                ("hovers".to_string(), Ok(())),
                ("formatting".to_string(), Ok(())),
            ]
        );
        assert_eq!(lang.error.as_deref(), Some("Error: no formatter found"));
        assert_eq!(rows[2].code, Code::NotStarted("no license".into()));

        // The list, as it is read.
        let page = report(&rows);
        assert!(page.contains("The 3 selected extensions, each installed and started."));
        assert!(page.contains("of the 3 with code, 2 started"));
        assert!(page.contains("| Acme.lang 1.0.0 | 500 |"));
        assert!(
            page.contains("hovers, formatting for python; hovers answered; formatting answered")
        );
        assert!(page.contains("its start failed: no license"));
        assert!(page.contains("| comments.createCommentController |"));
        // One the catalog lists and does not have is said, and the rest go on.
        let gone = Row {
            id: "Acme.gone".into(),
            code: Code::Failed("The catalog answered 404 | Not Found".into()),
            ..Default::default()
        };
        assert!(report(&[gone]).contains(r"404 \| Not Found"));
    }
}
