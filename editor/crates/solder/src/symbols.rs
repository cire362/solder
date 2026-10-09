//! The symbols of a file and of the project: what is declared there, by
//! name, to go to.
//!
//! They come from the language server. For a file whose language has no
//! server that lists them, they come from the language's own outline, so
//! the list is there for any file Solder can read. An extension that
//! brought the server may paint them, as it paints completions: Ruby's
//! colors the name of a class as one is colored where it is declared.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use extension::host::Symbol;
use gpui::{
    AnyElement, App, Context, DismissEvent, SharedString, Task, WeakEntity, Window, div,
    prelude::*, px,
};
use lsp::{Encoding, types as lt};

use crate::{
    completion::Label,
    extension_store::ExtensionStore,
    fuzzy,
    lsp_store::{Asked, LspStore},
    picker::{Picker, PickerDelegate, highlighted_text},
    theme::{ActiveTheme, Theme, UI_FONT_SIZE, UI_FONT_SMALL},
    workspace::{Jump, Workspace},
};

/// How many of a list's symbols are kept. A project has more than anyone
/// reads through, and the list narrows as its name is typed.
const MOST: usize = 2000;

/// One symbol in a list.
pub struct Row {
    pub name: String,
    /// The protocol's number for what it is, where a server said.
    kind: Option<i32>,
    /// What stands after the name: what it is in, or the file it is in.
    pub detail: String,
    pub path: PathBuf,
    jump: Jump,
    /// The server that named it, whose extension may paint it.
    server: Option<&'static str>,
    label: Option<Label>,
}

fn number(kind: lt::SymbolKind) -> Option<i32> {
    serde_json::to_value(kind)
        .ok()?
        .as_i64()
        .map(|kind| kind as i32)
}

/// A letter for what a symbol is, in the color of such a thing.
fn badge(kind: Option<i32>, theme: &Theme) -> (&'static str, gpui::Hsla) {
    match kind {
        Some(6 | 9 | 12) => ("ƒ", theme.syntax.function),
        Some(7 | 8 | 13 | 20) => ("v", theme.syntax.variable),
        Some(5 | 10 | 11 | 23 | 26) => ("T", theme.syntax.r#type),
        Some(1..=4) => ("m", theme.syntax.r#type),
        Some(14 | 22) => ("c", theme.syntax.number),
        _ => ("·", theme.fg_subtle),
    }
}

/// The symbols a server lists for the file at `path`. A server answers
/// with a tree of them or with a plain list; here they are a list either
/// way, each with what it is inside of.
pub fn of_document(
    response: lt::DocumentSymbolResponse,
    path: &Path,
    encoding: Encoding,
    server: &'static str,
) -> Vec<Row> {
    fn walk(
        symbols: Vec<lt::DocumentSymbol>,
        inside: &str,
        path: &Path,
        encoding: Encoding,
        server: &'static str,
        rows: &mut Vec<Row>,
    ) {
        for symbol in symbols {
            if rows.len() >= MOST {
                return;
            }
            let within = match inside.is_empty() {
                true => symbol.name.clone(),
                false => format!("{inside} \u{203a} {}", symbol.name),
            };
            rows.push(Row {
                name: symbol.name,
                kind: number(symbol.kind),
                detail: inside.to_string(),
                path: path.to_path_buf(),
                jump: Jump::Lsp {
                    range: symbol.selection_range,
                    encoding,
                },
                server: Some(server),
                label: None,
            });
            walk(
                symbol.children.unwrap_or_default(),
                &within,
                path,
                encoding,
                server,
                rows,
            );
        }
    }
    let mut rows = Vec::new();
    match response {
        lt::DocumentSymbolResponse::Nested(symbols) => {
            walk(symbols, "", path, encoding, server, &mut rows)
        }
        lt::DocumentSymbolResponse::Flat(symbols) => {
            rows.extend(symbols.into_iter().take(MOST).map(|symbol| Row {
                name: symbol.name,
                kind: number(symbol.kind),
                detail: symbol.container_name.unwrap_or_default(),
                path: path.to_path_buf(),
                jump: Jump::Lsp {
                    range: symbol.location.range,
                    encoding,
                },
                server: Some(server),
                label: None,
            }))
        }
    }
    rows
}

/// The symbols a server found in the project in `root`, each with the
/// file it is in.
pub fn of_workspace(
    response: lt::WorkspaceSymbolResponse,
    root: &Path,
    encoding: Encoding,
    server: &'static str,
) -> Vec<Row> {
    let row = |name: String,
               kind: lt::SymbolKind,
               inside: Option<String>,
               location: lt::Location|
     -> Option<Row> {
        let path = lsp::uri_to_path(&location.uri)?;
        let file = path.strip_prefix(root).unwrap_or(&path).display();
        let line = location.range.start.line + 1;
        Some(Row {
            name,
            kind: number(kind),
            detail: match inside.filter(|inside| !inside.is_empty()) {
                Some(inside) => format!("{inside}  {file}:{line}"),
                None => format!("{file}:{line}"),
            },
            path,
            jump: Jump::Lsp {
                range: location.range,
                encoding,
            },
            server: Some(server),
            label: None,
        })
    };
    match response {
        lt::WorkspaceSymbolResponse::Flat(symbols) => symbols
            .into_iter()
            .filter_map(|s| row(s.name, s.kind, s.container_name, s.location))
            .take(MOST)
            .collect(),
        lt::WorkspaceSymbolResponse::Nested(symbols) => symbols
            .into_iter()
            .filter_map(|s| match s.location {
                lt::OneOf::Left(location) => row(s.name, s.kind, s.container_name, location),
                // A file with no place in it yet: nowhere to go.
                lt::OneOf::Right(_) => None,
            })
            .take(MOST)
            .collect(),
    }
}

/// The symbols of a file by its language's outline, for one no server
/// lists them for.
pub fn of_outline(symbols: Vec<syntax::Symbol>, path: &Path) -> Vec<Row> {
    symbols
        .into_iter()
        .map(|symbol| Row {
            name: symbol.name,
            kind: match symbol.kind.as_ref() {
                "fn" | "def" | "function" | "func" => Some(12),
                "type" | "class" | "struct" | "enum" | "trait" | "interface" => Some(5),
                "const" => Some(14),
                _ => None,
            },
            detail: symbol.kind.into_owned(),
            path: path.to_path_buf(),
            // An outline counts lines from one.
            jump: Jump::Point {
                row: symbol.line.saturating_sub(1),
                column: 0,
            },
            server: None,
            label: None,
        })
        .collect()
}

/// Whose symbols a list is of.
enum Scope {
    /// One file's: all of them are there once they are loaded.
    File,
    /// The project's: the servers are asked again as the name is typed.
    Project(PathBuf),
}

pub struct Symbols {
    workspace: WeakEntity<Workspace>,
    scope: Scope,
    rows: Vec<Row>,
    loading: bool,
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
    /// Counts the lists there were, so that labels painted for one are
    /// not put on the next.
    list: u64,
    painting: Option<Task<()>>,
}

impl Symbols {
    pub fn of_file(workspace: WeakEntity<Workspace>) -> Self {
        Self::new(workspace, Scope::File)
    }

    pub fn of_project(workspace: WeakEntity<Workspace>, root: PathBuf) -> Self {
        Self::new(workspace, Scope::Project(root))
    }

    fn new(workspace: WeakEntity<Workspace>, scope: Scope) -> Self {
        Self {
            workspace,
            loading: matches!(scope, Scope::File),
            scope,
            rows: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            list: 0,
            painting: None,
        }
    }

    /// Fills a file's list: from the server that was asked, or with no
    /// server or no answer, from the outline of `source`.
    pub fn load_file(
        asked: Option<Asked<lt::request::DocumentSymbolRequest>>,
        path: PathBuf,
        source: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        cx.spawn_in(window, async move |picker, cx| {
            let mut rows = Vec::new();
            if let Some((server, encoding, request)) = asked
                && let Ok(Some(response)) = request.await
            {
                rows = of_document(response, &path, encoding, server);
            }
            if rows.is_empty() {
                let file = path.clone();
                rows = cx
                    .background_executor()
                    .spawn(async move {
                        of_outline(syntax::outline(&file, &source, MOST, || false), &file)
                    })
                    .await;
            }
            picker
                .update_in(cx, |picker, window, cx| {
                    picker.delegate.set_rows(rows, cx);
                    picker.refresh(window, cx);
                })
                .ok();
        })
        .detach();
    }

    fn set_rows(&mut self, rows: Vec<Row>, cx: &mut Context<Picker<Self>>) {
        self.rows = rows;
        self.loading = false;
        self.list += 1;
        self.paint(cx);
    }

    /// Asks the extensions that brought the servers how to show these
    /// symbols. The rows are there meanwhile, as the servers named them.
    fn paint(&mut self, cx: &mut Context<Picker<Self>>) {
        let Some(store) = ExtensionStore::try_global(cx) else {
            return;
        };
        let mut by_server: Vec<(&'static str, Vec<usize>)> = Vec::new();
        for (ix, row) in self.rows.iter().enumerate() {
            let Some(server) = row.server else { continue };
            match by_server.iter_mut().find(|(name, _)| *name == server) {
                Some((_, rows)) => rows.push(ix),
                None => by_server.push((server, vec![ix])),
            }
        }
        let mut asked = Vec::new();
        for (server, rows) in by_server {
            let symbols = rows
                .iter()
                .map(|ix| Symbol {
                    name: self.rows[*ix].name.clone(),
                    kind: self.rows[*ix].kind.unwrap_or(0),
                })
                .collect();
            // The label's code is colored with the grammar of its file.
            let languages: Vec<Option<Arc<syntax::Language>>> = rows
                .iter()
                .map(|ix| syntax::language_for_path(&self.rows[*ix].path))
                .collect();
            if let Some(labels) = store.read(cx).symbol_labels(server, symbols, cx) {
                asked.push((rows, languages, labels));
            }
        }
        if asked.is_empty() {
            return;
        }
        let list = self.list;
        self.painting = Some(cx.spawn(async move |picker, cx| {
            for (rows, languages, labels) in asked {
                let labels = labels.await;
                let painted: Vec<Option<Label>> = cx
                    .background_executor()
                    .spawn(async move {
                        labels
                            .iter()
                            .zip(&languages)
                            .map(|(label, language)| {
                                Some(Label::paint(label.as_ref()?, language.as_ref()))
                            })
                            .collect()
                    })
                    .await;
                picker
                    .update(cx, |picker, cx| {
                        if picker.delegate.list != list {
                            return;
                        }
                        for (ix, label) in rows.into_iter().zip(painted) {
                            if let Some(row) = picker.delegate.rows.get_mut(ix) {
                                row.label = label;
                            }
                        }
                        cx.notify();
                    })
                    .ok();
            }
        }));
    }

    fn filter(&mut self, query: &str) {
        self.matches = if query.is_empty() {
            (0..self.rows.len()).map(|ix| (ix, Vec::new())).collect()
        } else {
            fuzzy::fuzzy_match(
                self.rows.iter().map(|row| row.name.as_str()),
                query,
                200,
                false,
            )
            .into_iter()
            .map(|m| {
                (
                    m.index,
                    fuzzy::positions(&self.rows[m.index].name, query, false),
                )
            })
            .collect()
        };
        // A symbol named exactly so comes first, then the ones that
        // begin so; within each, the order of the match stands.
        let typed = query.to_lowercase();
        self.matches.sort_by_key(|(ix, _)| {
            let name = self.rows[*ix].name.to_lowercase();
            match (name == typed, name.starts_with(&typed)) {
                (true, _) => 0,
                (false, true) => 1,
                (false, false) => 2,
            }
        });
        self.selected = 0;
    }

    #[cfg(test)]
    pub fn shown(&self) -> Vec<(String, String, Option<String>)> {
        self.matches
            .iter()
            .map(|(ix, _)| {
                let row = &self.rows[*ix];
                (
                    row.name.clone(),
                    row.detail.clone(),
                    row.label.as_ref().map(|label| label.text.clone()),
                )
            })
            .collect()
    }
}

impl PickerDelegate for Symbols {
    fn placeholder(&self) -> SharedString {
        match self.scope {
            Scope::File => "Go to a symbol of this file".into(),
            Scope::Project(_) => "Go to a symbol of the project".into(),
        }
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Context<Picker<Self>>) {
        self.selected = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let query = query.trim().to_string();
        let Scope::Project(root) = &self.scope else {
            self.filter(&query);
            return Task::ready(());
        };
        let root = root.clone();
        let asked = LspStore::global(cx)
            .map(|store| store.read(cx).workspace_symbols(&root, &query))
            .unwrap_or_default();
        if asked.is_empty() {
            self.rows.clear();
            self.matches.clear();
            self.loading = false;
            return Task::ready(());
        }
        self.loading = true;
        cx.spawn(async move |picker, cx| {
            let mut rows = Vec::new();
            for (server, encoding, request) in asked {
                if let Ok(Some(response)) = request.await {
                    rows.extend(of_workspace(response, &root, encoding, server));
                }
            }
            rows.truncate(MOST);
            picker
                .update(cx, |picker, cx| {
                    picker.delegate.set_rows(rows, cx);
                    // The servers matched the name their own way; here
                    // the rows are put in the order of the best match.
                    picker.delegate.filter(&query);
                    if picker.delegate.matches.is_empty() {
                        let all = picker.delegate.rows.len();
                        picker.delegate.matches = (0..all).map(|ix| (ix, Vec::new())).collect();
                    }
                    picker.matches_updated(cx);
                })
                .ok();
        })
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((ix, _)) = self.matches.get(self.selected) else {
            return;
        };
        let row = &self.rows[*ix];
        let (path, jump, workspace) = (row.path.clone(), row.jump.clone(), self.workspace.clone());
        cx.emit(DismissEvent);
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.open_path(path, Some(jump), window, cx)
                })
                .ok();
        });
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let (ix, positions) = &self.matches[ix];
        let row = &self.rows[*ix];
        let theme = cx.theme();
        let (letter, color) = badge(row.kind, theme);
        // A painted label shows the name among other words: the typed
        // letters are marked in it only where it shows the name as it is.
        let name = match &row.label {
            Some(label) if label.text.get(label.filter.clone()) == Some(row.name.as_str()) => {
                label.styled(positions, theme)
            }
            Some(label) => label.styled(&[], theme),
            None => highlighted_text(&row.name, positions, theme.fg, theme.accent),
        };
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .text_size(UI_FONT_SIZE)
            .child(div().w(px(14.)).flex_none().text_color(color).child(letter))
            .child(div().flex_none().text_color(theme.fg).child(name))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(UI_FONT_SMALL)
                    .text_color(theme.fg_subtle)
                    .child(row.detail.clone()),
            )
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        match (&self.scope, self.loading) {
            (_, true) => "Asking...".into(),
            (Scope::File, false) if self.rows.is_empty() => {
                "Nothing here lists what this file declares".into()
            }
            (Scope::Project(_), false) if self.rows.is_empty() => {
                "Type a name. A language server of the project has to be running".into()
            }
            _ => "No symbol of that name".into(),
        }
    }

    fn width(&self) -> gpui::Pixels {
        px(640.)
    }
}

/// The file a list of symbols is of, as the workspace hands it over.
pub fn file_of(
    document: &gpui::Entity<crate::document::Document>,
    cx: &App,
) -> Option<(PathBuf, String)> {
    let document = document.read(cx);
    let path = document.path()?.to_path_buf();
    Some((path, document.text().rope().to_string()))
}
