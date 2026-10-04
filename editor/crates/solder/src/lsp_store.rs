//! Language servers for every open document: starts them on demand, keeps
//! them in sync with edits and turns their diagnostics into squiggles.

use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::{FutureExt, StreamExt, future::BoxFuture};
use gpui::{
    App, AppContext, Context, Entity, EntityId, Global, SharedString, Subscription, Task,
    WeakEntity,
};
use lsp::{Encoding, LanguageServer, Notification, ServerCommand, path_to_uri, types as lt};
use text::{Buffer, Point};

use crate::{
    document::{Diagnostic, Document, DocumentEvent, Severity},
    settings::Settings,
};

/// A language server Solder knows how to start.
#[derive(Clone, Copy, Debug)]
pub struct ServerSpec {
    pub name: &'static str,
    program: &'static str,
    args: &'static [&'static str],
    /// Files that mark the project root for this server.
    root_markers: &'static [&'static str],
    install_hint: &'static str,
}

pub fn spec_for(language: &str) -> Option<ServerSpec> {
    Some(match language {
        "Rust" => ServerSpec {
            name: "rust-analyzer",
            program: "rust-analyzer",
            args: &[],
            root_markers: &["Cargo.toml"],
            install_hint: "rustup component add rust-analyzer",
        },
        "TypeScript" | "TSX" | "JavaScript" => ServerSpec {
            name: "typescript-language-server",
            program: "typescript-language-server",
            args: &["--stdio"],
            root_markers: &["tsconfig.json", "jsconfig.json", "package.json"],
            install_hint: "npm i -g typescript typescript-language-server",
        },
        "Go" => ServerSpec {
            name: "gopls",
            program: "gopls",
            args: &[],
            root_markers: &["go.mod"],
            install_hint: "go install golang.org/x/tools/gopls@latest",
        },
        "Python" => ServerSpec {
            name: "pyright",
            program: "pyright-langserver",
            args: &["--stdio"],
            root_markers: &["pyproject.toml", "setup.py", "requirements.txt"],
            install_hint: "npm i -g pyright",
        },
        "CSS" => ServerSpec {
            name: "css-language-server",
            program: "vscode-css-language-server",
            args: &["--stdio"],
            root_markers: &["package.json"],
            install_hint: "npm i -g vscode-langservers-extracted",
        },
        "JSON" => ServerSpec {
            name: "json-language-server",
            program: "vscode-json-language-server",
            args: &["--stdio"],
            root_markers: &["package.json"],
            install_hint: "npm i -g vscode-langservers-extracted",
        },
        _ => return None,
    })
}

/// Nearest ancestor of `file` holding one of `markers`, else its folder.
fn find_root(file: &Path, markers: &[&str]) -> PathBuf {
    let dir = file.parent().unwrap_or(file);
    dir.ancestors()
        .find(|d| markers.iter().any(|m| d.join(m).exists()))
        .unwrap_or(dir)
        .to_path_buf()
}

/// Project-local `node_modules/.bin` first, then `PATH`, then the usual
/// install locations (apps started from the Dock get a minimal `PATH`).
pub(crate) fn find_program(program: &str, root: &Path) -> Option<PathBuf> {
    for dir in root.ancestors() {
        let local = dir.join("node_modules/.bin").join(program);
        if local.is_file() {
            return Some(local);
        }
    }
    let home = dirs::home_dir().unwrap_or_default();
    let extra = [
        home.join(".cargo/bin"),
        home.join(".local/bin"),
        home.join("go/bin"),
        home.join("Library/pnpm"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .chain(extra)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ServerKey {
    name: &'static str,
    root: PathBuf,
}

enum ServerState {
    Starting,
    Running {
        server: Arc<LanguageServer>,
        _notifications: Task<()>,
    },
    Failed,
}

struct DocEntry {
    document: WeakEntity<Document>,
    key: Option<ServerKey>,
    uri: lt::Uri,
    version: i32,
    opened: bool,
    _subscriptions: [Subscription; 2],
}

#[derive(Default)]
pub struct LspStore {
    servers: HashMap<ServerKey, ServerState>,
    docs: HashMap<EntityId, DocEntry>,
    /// Latest progress or error message, shown in the status bar.
    status: Option<SharedString>,
}

/// A server asked the client to apply an edit (usually after a code action
/// command). Workspaces apply the parts under their root.
pub enum LspStoreEvent {
    ApplyEdit {
        edit: lt::WorkspaceEdit,
        encoding: Encoding,
    },
}

impl gpui::EventEmitter<LspStoreEvent> for LspStore {}

struct GlobalLspStore(Entity<LspStore>);

impl Global for GlobalLspStore {}

pub fn init(cx: &mut App) {
    let store = cx.new(|_| LspStore::default());
    cx.on_app_quit({
        let store = store.clone();
        move |cx| {
            let servers = store.update(cx, |s, _| s.shutdown_all());
            async move {
                // Language servers are child processes; without this they
                // would outlive the editor.
                for server in servers {
                    server.kill();
                }
            }
        }
    })
    .detach();
    cx.set_global(GlobalLspStore(store));
}

impl LspStore {
    pub fn global(cx: &App) -> Option<Entity<LspStore>> {
        cx.try_global::<GlobalLspStore>().map(|g| g.0.clone())
    }

    /// Starts tracking a document; opens it on its server once that is up.
    pub fn register(document: &Entity<Document>, cx: &mut App) {
        if let Some(store) = Self::global(cx) {
            store.update(cx, |s, cx| s.register_document(document, cx));
        }
    }

    pub fn status(&self) -> Option<&SharedString> {
        self.status.as_ref()
    }

    fn register_document(&mut self, document: &Entity<Document>, cx: &mut Context<Self>) {
        let id = document.entity_id();
        if self.docs.contains_key(&id) {
            return;
        }
        let subscriptions = [
            cx.subscribe(document, |this, doc, event, cx| match event {
                DocumentEvent::Edited { edits, .. } => this.did_change(doc.entity_id(), edits, cx),
                DocumentEvent::Saved => this.did_save(doc.entity_id()),
                DocumentEvent::PathChanged => {
                    // Close under the old name, open under the new one.
                    this.close(doc.entity_id());
                    this.attach(doc.entity_id(), cx);
                }
                DocumentEvent::DirtyChanged
                | DocumentEvent::DiagnosticsChanged
                | DocumentEvent::GitChanged => {}
            }),
            cx.observe_release(document, move |this, _, _| {
                this.close(id);
                this.docs.remove(&id);
            }),
        ];
        self.docs.insert(
            id,
            DocEntry {
                document: document.downgrade(),
                key: None,
                uri: path_to_uri(Path::new("/")),
                version: 0,
                opened: false,
                _subscriptions: subscriptions,
            },
        );
        self.attach(id, cx);
    }

    /// Works out which server a document belongs to and gets it opened there.
    fn attach(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(entry) = self.docs.get_mut(&id) else {
            return;
        };
        let Some(document) = entry.document.upgrade() else {
            return;
        };
        let doc = document.read(cx);
        let (Some(path), Some(language)) = (doc.path(), doc.language_name()) else {
            entry.key = None;
            return;
        };
        let Some(spec) = spec_for(language) else {
            entry.key = None;
            return;
        };
        let settings = Settings::get(cx).language_servers.get(spec.name).cloned();
        if settings.as_ref().is_some_and(|s| s.disabled) {
            entry.key = None;
            return;
        }
        let key = ServerKey {
            name: spec.name,
            root: find_root(path, spec.root_markers),
        };
        entry.uri = path_to_uri(path);
        entry.key = Some(key.clone());
        entry.opened = false;
        match self.servers.get(&key) {
            Some(ServerState::Running { .. }) => self.open(id, cx),
            Some(ServerState::Starting | ServerState::Failed) => {}
            None => self.start(key, spec, cx),
        }
    }

    fn start(&mut self, key: ServerKey, spec: ServerSpec, cx: &mut Context<Self>) {
        let overrides = Settings::get(cx)
            .language_servers
            .get(spec.name)
            .cloned()
            .unwrap_or_default();
        let program = match overrides.command.as_deref() {
            Some(cmd) => Some(PathBuf::from(cmd)),
            None => find_program(spec.program, &key.root),
        };
        let Some(program) = program else {
            let message = format!(
                "{} not found. Install it: {}",
                spec.program, spec.install_hint
            );
            self.status = Some(message.clone().into());
            self.servers.insert(key, ServerState::Failed);
            cx.notify();
            return;
        };
        let command = ServerCommand {
            program,
            args: overrides
                .args
                .unwrap_or_else(|| spec.args.iter().map(|a| a.to_string()).collect()),
            env: Vec::new(),
        };
        self.servers.insert(key.clone(), ServerState::Starting);
        self.status = Some(format!("Starting {}...", spec.name).into());
        cx.notify();
        let options = overrides.initialization_options;
        cx.spawn(async move |this, cx| {
            let root = key.root.clone();
            let spawned = cx
                .background_executor()
                .spawn(async move { LanguageServer::spawn(spec.name, &command, &root) })
                .await;
            let result = match spawned {
                Ok((server, notifications)) => server
                    .initialize(&key.root, options)
                    .await
                    .map(|_| (server, notifications)),
                Err(e) => Err(e),
            };
            this.update(cx, |this, cx| match result {
                Ok((server, notifications)) => this.started(key, server, notifications, cx),
                Err(err) => {
                    let message = format!(
                        "{} failed to start: {err}. {}",
                        spec.name, spec.install_hint
                    );
                    this.status = Some(message.clone().into());
                    this.servers.insert(key, ServerState::Failed);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn started(
        &mut self,
        key: ServerKey,
        server: Arc<LanguageServer>,
        mut notifications: futures::channel::mpsc::UnboundedReceiver<Notification>,
        cx: &mut Context<Self>,
    ) {
        let pump_key = key.clone();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(n) = notifications.next().await {
                if this
                    .update(cx, |this, cx| this.handle_notification(&pump_key, n, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.status = None;
        self.servers.insert(
            key.clone(),
            ServerState::Running {
                server,
                _notifications: pump,
            },
        );
        let waiting: Vec<EntityId> = self
            .docs
            .iter()
            .filter(|(_, e)| e.key.as_ref() == Some(&key) && !e.opened)
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            self.open(id, cx);
        }
        cx.notify();
    }

    fn server(&self, key: &ServerKey) -> Option<&Arc<LanguageServer>> {
        match self.servers.get(key) {
            Some(ServerState::Running { server, .. }) => Some(server),
            _ => None,
        }
    }

    fn open(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(entry) = self.docs.get(&id) else {
            return;
        };
        let (Some(key), Some(document)) = (entry.key.clone(), entry.document.upgrade()) else {
            return;
        };
        let Some(server) = self.server(&key).cloned() else {
            return;
        };
        let doc = document.read(cx);
        server.notify::<lt::notification::DidOpenTextDocument>(lt::DidOpenTextDocumentParams {
            text_document: lt::TextDocumentItem {
                uri: entry.uri.clone(),
                language_id: doc.language_id().into(),
                version: 0,
                text: doc.text().rope().to_string(),
            },
        });
        if let Some(entry) = self.docs.get_mut(&id) {
            entry.opened = true;
            entry.version = 0;
        }
    }

    fn did_change(&mut self, id: EntityId, edits: &Arc<[text::Edit]>, cx: &mut Context<Self>) {
        let Some(entry) = self.docs.get_mut(&id) else {
            return;
        };
        if !entry.opened {
            return;
        }
        let Some(key) = entry.key.clone() else { return };
        let Some(server) = (match self.servers.get(&key) {
            Some(ServerState::Running { server, .. }) => Some(server.clone()),
            _ => None,
        }) else {
            return;
        };
        let sync = sync_kind(&server.capabilities());
        if sync == lt::TextDocumentSyncKind::NONE {
            return;
        }
        entry.version += 1;
        let changes = if sync == lt::TextDocumentSyncKind::INCREMENTAL {
            let utf8 = server.encoding() == Encoding::Utf8;
            edits
                .iter()
                .map(|e| {
                    let (start, end) = if utf8 {
                        (e.start_point, e.old_end_point)
                    } else {
                        (e.start_utf16, e.old_end_utf16)
                    };
                    lt::TextDocumentContentChangeEvent {
                        range: Some(lt::Range::new(position(start), position(end))),
                        range_length: None,
                        text: e.new_text.to_string(),
                    }
                })
                .collect()
        } else {
            let Some(document) = entry.document.upgrade() else {
                return;
            };
            vec![lt::TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: document.read(cx).text().rope().to_string(),
            }]
        };
        server.notify::<lt::notification::DidChangeTextDocument>(lt::DidChangeTextDocumentParams {
            text_document: lt::VersionedTextDocumentIdentifier {
                uri: entry.uri.clone(),
                version: entry.version,
            },
            content_changes: changes,
        });
    }

    fn did_save(&mut self, id: EntityId) {
        let Some(entry) = self.docs.get(&id) else {
            return;
        };
        let (true, Some(key)) = (entry.opened, entry.key.as_ref()) else {
            return;
        };
        if let Some(server) = self.server(key) {
            server.notify::<lt::notification::DidSaveTextDocument>(lt::DidSaveTextDocumentParams {
                text_document: lt::TextDocumentIdentifier {
                    uri: entry.uri.clone(),
                },
                text: None,
            });
        }
    }

    fn close(&mut self, id: EntityId) {
        let Some(entry) = self.docs.get_mut(&id) else {
            return;
        };
        if !entry.opened {
            return;
        }
        entry.opened = false;
        if let Some(server) = entry.key.as_ref().and_then(|k| match self.servers.get(k) {
            Some(ServerState::Running { server, .. }) => Some(server),
            _ => None,
        }) {
            server.notify::<lt::notification::DidCloseTextDocument>(
                lt::DidCloseTextDocumentParams {
                    text_document: lt::TextDocumentIdentifier {
                        uri: entry.uri.clone(),
                    },
                },
            );
        }
    }

    fn handle_notification(&mut self, key: &ServerKey, n: Notification, cx: &mut Context<Self>) {
        match n.method.as_str() {
            "textDocument/publishDiagnostics" => {
                let Ok(params) = serde_json::from_value::<lt::PublishDiagnosticsParams>(n.params)
                else {
                    return;
                };
                let Some(server) = self.server(key).cloned() else {
                    return;
                };
                let encoding = server.encoding();
                for entry in self.docs.values() {
                    if entry.key.as_ref() != Some(key) || entry.uri != params.uri {
                        continue;
                    }
                    let Some(document) = entry.document.upgrade() else {
                        continue;
                    };
                    let diagnostics = params.diagnostics.clone();
                    document.update(cx, |doc, cx| {
                        let converted = diagnostics
                            .iter()
                            .map(|d| Diagnostic {
                                range: from_range(doc.text(), d.range, encoding),
                                severity: match d.severity {
                                    Some(lt::DiagnosticSeverity::ERROR) => Severity::Error,
                                    Some(lt::DiagnosticSeverity::WARNING) => Severity::Warning,
                                    Some(lt::DiagnosticSeverity::HINT) => Severity::Hint,
                                    _ => Severity::Info,
                                },
                                message: d.message.clone(),
                                source: d.source.clone(),
                            })
                            .collect();
                        doc.set_diagnostics(converted, cx);
                    });
                }
            }
            "$/progress" => {
                let Ok(params) = serde_json::from_value::<lt::ProgressParams>(n.params) else {
                    return;
                };
                let lt::ProgressParamsValue::WorkDone(progress) = params.value;
                self.status = match progress {
                    lt::WorkDoneProgress::Begin(b) => Some(progress_text(
                        key.name,
                        &b.title,
                        b.message.as_deref(),
                        b.percentage,
                    )),
                    lt::WorkDoneProgress::Report(r) => Some(progress_text(
                        key.name,
                        "",
                        r.message.as_deref(),
                        r.percentage,
                    )),
                    lt::WorkDoneProgress::End(_) => None,
                };
                cx.notify();
            }
            "workspace/applyEdit" => {
                let Some(server) = self.server(key).cloned() else {
                    return;
                };
                let id = n.id.unwrap_or(serde_json::Value::Null);
                match serde_json::from_value::<lt::ApplyWorkspaceEditParams>(n.params) {
                    Ok(params) => {
                        cx.emit(LspStoreEvent::ApplyEdit {
                            edit: params.edit,
                            encoding: server.encoding(),
                        });
                        server.respond(id, serde_json::json!({ "applied": true }));
                    }
                    Err(e) => server.respond(
                        id,
                        serde_json::json!({ "applied": false, "failureReason": e.to_string() }),
                    ),
                }
            }
            "window/showMessage" => {
                if let Ok(params) = serde_json::from_value::<lt::ShowMessageParams>(n.params)
                    && params.typ == lt::MessageType::ERROR
                {
                    self.status = Some(format!("{}: {}", key.name, params.message).into());
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// Sends a request about `document` to its server. `None` when the
    /// document has no running server.
    pub fn request<R: lt::request::Request>(
        &self,
        document: &Entity<Document>,
        cx: &App,
        params: impl FnOnce(lt::TextDocumentIdentifier, Encoding, &Buffer) -> R::Params,
    ) -> Option<(Encoding, BoxFuture<'static, lsp::Result<R::Result>>)> {
        let entry = self.docs.get(&document.entity_id())?;
        if !entry.opened {
            return None;
        }
        let server = self.server(entry.key.as_ref()?)?;
        let encoding = server.encoding();
        let id = lt::TextDocumentIdentifier {
            uri: entry.uri.clone(),
        };
        let params = params(id, encoding, document.read(cx).text());
        Some((encoding, server.request::<R>(params).boxed()))
    }

    /// Sends a request to the document's server that is not about a position
    /// in the document (code action resolve, execute command).
    pub fn server_request<R: lt::request::Request>(
        &self,
        document: &Entity<Document>,
        params: R::Params,
    ) -> Option<BoxFuture<'static, lsp::Result<R::Result>>> {
        let entry = self.docs.get(&document.entity_id())?;
        let server = self.server(entry.key.as_ref()?)?;
        Some(server.request::<R>(params).boxed())
    }

    pub fn capabilities(&self, document: &Entity<Document>) -> Option<lt::ServerCapabilities> {
        let entry = self.docs.get(&document.entity_id())?;
        Some(self.server(entry.key.as_ref()?)?.capabilities())
    }

    /// Stops every server. Called when the app quits.
    pub fn shutdown_all(&mut self) -> Vec<Arc<LanguageServer>> {
        self.servers
            .drain()
            .filter_map(|(_, s)| match s {
                ServerState::Running { server, .. } => Some(server),
                _ => None,
            })
            .collect()
    }
}

fn progress_text(
    name: &str,
    title: &str,
    message: Option<&str>,
    percent: Option<u32>,
) -> SharedString {
    let mut text = format!("{name}: {title}");
    if let Some(m) = message {
        if !title.is_empty() {
            text.push(' ');
        }
        text.push_str(m);
    }
    if let Some(p) = percent {
        text.push_str(&format!(" {p}%"));
    }
    text.into()
}

fn sync_kind(caps: &lt::ServerCapabilities) -> lt::TextDocumentSyncKind {
    match &caps.text_document_sync {
        Some(lt::TextDocumentSyncCapability::Kind(k)) => *k,
        Some(lt::TextDocumentSyncCapability::Options(o)) => {
            o.change.unwrap_or(lt::TextDocumentSyncKind::NONE)
        }
        None => lt::TextDocumentSyncKind::NONE,
    }
}

fn position(p: Point) -> lt::Position {
    lt::Position::new(p.row as u32, p.column as u32)
}

pub fn to_position(buffer: &Buffer, offset: usize, encoding: Encoding) -> lt::Position {
    match encoding {
        Encoding::Utf8 => position(buffer.offset_to_point(offset)),
        Encoding::Utf16 => position(buffer.offset_to_utf16(offset)),
    }
}

pub fn to_offset(buffer: &Buffer, pos: lt::Position, encoding: Encoding) -> usize {
    let point = Point::new(pos.line as usize, pos.character as usize);
    match encoding {
        Encoding::Utf8 => buffer.point_to_offset(point),
        Encoding::Utf16 => buffer.utf16_to_offset(point),
    }
}

pub fn from_range(buffer: &Buffer, range: lt::Range, encoding: Encoding) -> Range<usize> {
    let start = to_offset(buffer, range.start, encoding);
    let end = to_offset(buffer, range.end, encoding);
    start.min(end)..end.max(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_nearest_marker() {
        let dir = db::testing::dir("root");
        std::fs::create_dir_all(dir.join("crates/a/src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("crates/a/Cargo.toml"), "").unwrap();
        let file = dir.join("crates/a/src/lib.rs");
        assert_eq!(find_root(&file, &["Cargo.toml"]), dir.join("crates/a"));
        assert_eq!(find_root(&file, &["go.mod"]), dir.join("crates/a/src"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn positions_convert_in_both_encodings() {
        let b = Buffer::new("é😀x\nab");
        // "x" is byte 6, UTF-16 column 3.
        assert_eq!(to_position(&b, 6, Encoding::Utf8), lt::Position::new(0, 6));
        assert_eq!(to_position(&b, 6, Encoding::Utf16), lt::Position::new(0, 3));
        assert_eq!(to_offset(&b, lt::Position::new(0, 3), Encoding::Utf16), 6);
        assert_eq!(to_offset(&b, lt::Position::new(1, 1), Encoding::Utf8), 9);
    }
}
