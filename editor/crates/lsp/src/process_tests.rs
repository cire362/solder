use super::*;
use futures::{StreamExt, executor::block_on};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Fixture {
    directory: PathBuf,
    root: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("solder-lsp-map-{}-{id}", std::process::id()));
        let root = directory.join("cache 日本語 %");
        std::fs::create_dir_all(&root).unwrap();
        Self {
            log: directory.join("wire.jsonl"),
            directory,
            root,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

// A failed wire assertion closes the server, while a hung process is killed
// before joining the waiter. Neither failure can leave a test waiting forever.
fn wait<T: Send>(server: &LanguageServer, future: impl Future<Output = T> + Send) -> T {
    std::thread::scope(|scope| {
        let (tx, rx) = std_mpsc::channel();
        scope.spawn(move || {
            let _ = tx.send(block_on(future));
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(value) => value,
            Err(error) => {
                server.kill();
                panic!("mock language server did not answer: {error}");
            }
        }
    })
}

fn ask<R: lsp_types::request::Request>(server: &LanguageServer, params: Value) -> R::Result {
    wait(
        server,
        server.request::<R>(serde_json::from_value(params).unwrap()),
    )
    .unwrap()
}

fn check_process(remote: Option<&str>) {
    let fixture = Fixture::new();
    let uri = path_to_uri(&fixture.root.join("a 日本語 %.rs"));
    let path_map = remote.map(|remote| PathMap::new(&fixture.root, remote).unwrap());
    let wire_root = path_map.as_ref().map_or_else(
        || path_to_uri(&fixture.root),
        |map| map.to_remote_uri(&path_to_uri(&fixture.root)),
    );
    let wire_uri = path_map
        .as_ref()
        .map_or_else(|| uri.clone(), |map| map.to_remote_uri(&uri));
    let root_path = remote
        .map(|root| root.trim_end_matches('/'))
        .map(|root| if root.is_empty() { "/" } else { root });
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let command = ServerCommand {
        program: "python3".into(),
        args: vec![
            crate_root
                .join("tests/fixtures/mapped_lsp.py")
                .to_str()
                .unwrap()
                .into(),
            crate_root
                .join("../solder/tests/fixtures/mock_lsp.py")
                .to_str()
                .unwrap()
                .into(),
        ],
        env: vec![
            ("MAPPED_LSP_ROOT_URI".into(), wire_root.as_str().into()),
            (
                "MAPPED_LSP_ROOT_PATH".into(),
                serde_json::to_string(&root_path).unwrap(),
            ),
            ("MAPPED_LSP_DOCUMENT_URI".into(), wire_uri.as_str().into()),
            (
                "MAPPED_LSP_WIRE_LOG".into(),
                fixture.log.to_str().unwrap().into(),
            ),
        ],
    };
    let (server, mut notes) = match path_map {
        Some(map) => LanguageServer::spawn_mapped("mapped mock", &command, &fixture.root, map),
        None => LanguageServer::spawn("local mock", &command, &fixture.root),
    }
    .unwrap();
    server.set_configuration(json!({"demo": {"uri": uri, "text": uri}}));
    wait(
        &server,
        server.initialize(&fixture.root, Some(json!({"uri": uri}))),
    )
    .unwrap();
    assert_eq!(server.encoding(), Encoding::Utf8);
    let outside = wait(&server, notes.next()).unwrap();
    assert_eq!(outside.params["uri"], "file://other-machine/outside.rs");

    let text = format!("fn helper() {{}}\n// TODO {}\n", uri.as_str());
    server.notify::<lsp_types::notification::DidOpenTextDocument>(
        lsp_types::DidOpenTextDocumentParams {
            text_document: lsp_types::TextDocumentItem {
                uri: uri.clone(),
                language_id: "rust".into(),
                version: 1,
                text: text.clone(),
            },
        },
    );
    let diagnostics = wait(&server, notes.next()).unwrap();
    assert_eq!(diagnostics.params["uri"], uri.as_str());
    assert_eq!(
        diagnostics.params["diagnostics"][0]["message"],
        "found TODO"
    );

    let changed = format!("fn helper() {{ helper(); }}\n// boom {}\n", uri.as_str());
    server.notify::<lsp_types::notification::DidChangeTextDocument>(
        lsp_types::DidChangeTextDocumentParams {
            text_document: lsp_types::VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 2,
            },
            content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: changed.clone(),
            }],
        },
    );
    let diagnostics = wait(&server, notes.next()).unwrap();
    assert_eq!(diagnostics.params["uri"], uri.as_str());
    assert_eq!(
        diagnostics.params["diagnostics"][0]["message"],
        "found boom"
    );

    let position = json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 5}});
    let completion = ask::<lsp_types::request::Completion>(&server, position.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(completion).unwrap()[0]["label"],
        "println"
    );
    let definition = ask::<lsp_types::request::GotoDefinition>(&server, position.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(definition).unwrap()["uri"],
        uri.as_str()
    );
    let mut params = position.clone();
    params["context"] = json!({"includeDeclaration": true});
    let references = ask::<lsp_types::request::References>(&server, params).unwrap();
    assert_eq!(references.len(), 2);
    assert!(references.iter().all(|location| location.uri == uri));
    let document_symbols = ask::<lsp_types::request::DocumentSymbolRequest>(
        &server,
        json!({"textDocument": {"uri": uri}}),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(document_symbols).unwrap()[0]["children"][0]["name"],
        "helper"
    );
    let symbols =
        ask::<lsp_types::request::WorkspaceSymbolRequest>(&server, json!({"query": "helper"}))
            .unwrap();
    assert_eq!(
        serde_json::to_value(symbols).unwrap()[0]["location"]["uri"],
        uri.as_str()
    );

    let mut rename = position;
    rename["newName"] = json!(uri.as_str());
    let edit = ask::<lsp_types::request::Rename>(&server, rename).unwrap();
    let changes = serde_json::to_value(edit).unwrap()["changes"].clone();
    assert_eq!(changes.as_object().unwrap().len(), 1);
    assert_eq!(changes[uri.as_str()].as_array().unwrap().len(), 2);
    assert!(
        changes[uri.as_str()]
            .as_array()
            .unwrap()
            .iter()
            .all(|edit| edit["newText"] == uri.as_str())
    );

    // Arguments are opaque. A URI the server supplied there must travel
    // back verbatim; command text that looks like a URI is also unchanged.
    ask::<lsp_types::request::ExecuteCommand>(
        &server,
        json!({"command": uri, "arguments": [wire_uri]}),
    );
    let applying = wait(&server, notes.next()).unwrap();
    assert_eq!(applying.method, "workspace/applyEdit");
    assert_eq!(applying.id, Some(json!(9001)));
    assert_eq!(
        applying.params["edit"]["changes"][uri.as_str()][0]["newText"],
        "// touched\n"
    );
    server.respond(applying.id.unwrap(), json!({"applied": true, "uri": uri}));
    wait(&server, server.shutdown());

    let messages: Vec<Value> = std::fs::read_to_string(&fixture.log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let message = |method: &str| {
        messages
            .iter()
            .find(|message| message["method"] == method)
            .unwrap()
    };
    let initialization = message("initialize");
    assert_eq!(
        initialization["params"]["initializationOptions"]["uri"],
        uri.as_str()
    );
    assert_eq!(
        initialization["params"]["workspaceFolders"][0]["name"],
        "cache 日本語 %"
    );
    assert_eq!(
        message("textDocument/didOpen")["params"]["textDocument"]["text"],
        text
    );
    assert_eq!(
        message("textDocument/didChange")["params"]["contentChanges"][0]["text"],
        changed
    );
    assert_eq!(
        message("workspace/executeCommand")["params"]["command"],
        uri.as_str()
    );
    let configuration = messages
        .iter()
        .find(|message| message["id"] == format!("{}/configuration", wire_root.as_str()))
        .unwrap();
    assert_eq!(
        configuration["result"],
        json!([{ "uri": wire_uri, "text": uri }])
    );
    assert!(
        messages
            .iter()
            .any(|message| message["id"] == 9001 && message["result"]["uri"] == wire_uri.as_str())
    );
    server.kill();
}

#[test]
fn mapped_stdio_server_translates_handshake_documents_features_and_both_kinds_of_reply() {
    for remote in ["/srv/project 日本語 %", "/"] {
        check_process(Some(remote));
    }
}

#[test]
fn ordinary_spawn_keeps_the_existing_local_protocol() {
    check_process(None);
}
