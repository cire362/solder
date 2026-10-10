//! Decorations belong to their extension, so disposing a type or stopping
//! its host removes only its marks. File providers see visible entries only.

use crate::{extension_api::ExtensionEvent, extension_store::ExtensionStore, lsp_store::to_offset};
use gpui::{App, Context};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Decoration {
    pub range: Range<usize>,
    pub background: Value,
    pub whole_line: bool,
    pub before: Option<String>,
    pub after: Option<String>,
}
#[derive(Clone, Debug)]
pub struct FileMark {
    pub badge: String,
    pub tooltip: Option<String>,
}
#[derive(Default)]
pub struct Files {
    providers: BTreeSet<(String, u64)>,
    // Empty entries remember that a provider was asked and had no badge.
    marks: BTreeMap<PathBuf, BTreeMap<String, Vec<FileMark>>>,
    revision: usize,
}
impl ExtensionStore {
    pub(crate) fn decoration_said(
        &mut self,
        owner: &str,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
    ) {
        if method.starts_with("files.") {
            if method == "files.provider" {
                let key = (owner.to_string(), params["id"].as_u64().unwrap_or_default());
                if params["gone"] == true {
                    self.api.files.providers.remove(&key);
                } else {
                    self.api.files.providers.insert(key);
                }
            }
            self.api.files.marks.clear();
            self.api.files.revision += 1;
            cx.emit(ExtensionEvent::Files);
            return;
        }
        let Some(kind) = params["type"].as_str() else {
            return;
        };
        for document in self
            .documents
            .iter()
            .filter_map(|document| document.upgrade())
        {
            if method == "decorations.gone" {
                document.update(cx, |doc, cx| doc.clear_decorations(owner, Some(kind), cx));
                continue;
            }
            let doc = document.read(cx);
            if params["uri"]
                .as_str()
                .and_then(|uri| {
                    uri.parse::<lsp::types::Uri>()
                        .ok()
                        .and_then(|uri| lsp::uri_to_path(&uri))
                })
                .as_deref()
                != doc.path()
            {
                continue;
            }
            if params["version"]
                .as_u64()
                .is_some_and(|version| version != doc.version())
            {
                continue;
            }
            let option = &params["options"];
            let mut decorations = Vec::new();
            for range in params["ranges"]
                .as_array()
                .into_iter()
                .flatten()
                .take(10000)
            {
                let Ok(at) = serde_json::from_value::<lsp::types::Range>(range["range"].clone())
                else {
                    continue;
                };
                let start = to_offset(doc.text(), at.start, lsp::Encoding::Utf16);
                let end = to_offset(doc.text(), at.end, lsp::Encoding::Utf16);
                let words = |key: &str| {
                    range[key]
                        .as_str()
                        .or(option[key].as_str())
                        .map(|text| text.chars().take(1024).collect())
                };
                decorations.push(Decoration {
                    range: start.min(end)..end.max(start),
                    background: option["background"].clone(),
                    whole_line: option["wholeLine"] == true,
                    before: words("before"),
                    after: words("after"),
                });
            }
            document.update(cx, |doc, cx| {
                doc.set_decorations(owner, kind, decorations, cx)
            });
        }
    }
    pub(crate) fn clear_extension_decorations(&mut self, owner: &str, cx: &mut Context<Self>) {
        self.api.files.providers.retain(|(of, _)| of != owner);
        self.api.files.marks.clear();
        self.api.files.revision += 1;
        for document in self
            .documents
            .iter()
            .filter_map(|document| document.upgrade())
        {
            document.update(cx, |doc, cx| doc.clear_decorations(owner, None, cx));
        }
        cx.emit(ExtensionEvent::Files);
    }
    pub fn file_marks(&self, path: &Path) -> Vec<FileMark> {
        self.api
            .files
            .marks
            .get(path)
            .into_iter()
            .flat_map(|marks| marks.values().flatten())
            .cloned()
            .collect()
    }
    pub fn decorate_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let owners: BTreeSet<_> = self
            .api
            .files
            .providers
            .iter()
            .map(|(owner, _)| owner.clone())
            .collect();
        if owners.is_empty() {
            return;
        }
        if self.api.files.marks.len() > 4096 {
            self.api.files.marks.clear();
        }
        for owner in owners {
            let missing: Vec<_> = paths
                .iter()
                .take(256)
                .filter(|path| {
                    !self
                        .api
                        .files
                        .marks
                        .get(*path)
                        .is_some_and(|marks| marks.contains_key(&owner))
                })
                .cloned()
                .collect();
            if missing.is_empty() {
                continue;
            }
            for path in &missing {
                self.api
                    .files
                    .marks
                    .entry(path.clone())
                    .or_default()
                    .insert(owner.clone(), Vec::new());
            }
            let revision = self.api.files.revision;
            let uris: Vec<_> = missing
                .iter()
                .map(|path| lsp::path_to_uri(path).to_string())
                .collect();
            let task = self.ask_host(&owner, "files.decorate", json!({"uris": uris}), cx);
            cx.spawn(async move |this, cx| {
                let answer = task.await;
                this.update(cx, |this, cx| {
                    if revision != this.api.files.revision {
                        return;
                    }
                    if let Ok(answer) = answer {
                        for mark in answer.as_array().into_iter().flatten() {
                            let Some(path) = mark["uri"].as_str().and_then(|uri| {
                                uri.parse::<lsp::types::Uri>()
                                    .ok()
                                    .and_then(|uri| lsp::uri_to_path(&uri))
                            }) else {
                                continue;
                            };
                            if !missing.contains(&path) {
                                continue;
                            }
                            let Some(badge) = mark["badge"].as_str() else {
                                continue;
                            };
                            this.api
                                .files
                                .marks
                                .entry(path)
                                .or_default()
                                .entry(owner.clone())
                                .or_default()
                                .push(FileMark {
                                    badge: badge.chars().take(4).collect(),
                                    tooltip: mark["tooltip"].as_str().map(str::to_string),
                                });
                        }
                    }
                    cx.emit(ExtensionEvent::Files);
                })
                .ok();
            })
            .detach();
        }
    }
}

/// Scheduled after rendering to avoid updating a store that a row is reading.
pub fn ask_visible(paths: Vec<PathBuf>, cx: &mut App) {
    if let Some(store) = ExtensionStore::try_global(cx) {
        if store.read(cx).api.files.providers.is_empty() {
            return;
        }
        cx.defer(move |cx| store.update(cx, |store, cx| store.decorate_files(paths, cx)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, Inlay};
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn decorations_use_utf16_move_with_edits_and_keep_other_owners(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let root = db::testing::dir("extension-decorations");
        let path = root.join("example.txt");
        let document = cx.new(|cx| Document::new(Some(path.clone()), "a😀b", cx));
        let store =
            cx.new(|cx| ExtensionStore::new(root.join("extensions"), root.join("config"), cx));
        document.update(cx, |doc, cx| {
            doc.set_inlays(
                vec![Inlay {
                    offset: 6,
                    text: " server".into(),
                }],
                cx,
            )
        });
        store.update(cx, |store, cx| {
            store.documents.push(document.downgrade());
            let params = json!({"uri": lsp::path_to_uri(&path).to_string(), "type":"d1", "options":{"after":" extension"},
                "ranges":[{"range":{"start":{"line":0,"character":1},"end":{"line":0,"character":3}}}]});
            store.decoration_said("one", "decorations", params.clone(), cx);
            store.decoration_said("two", "decorations", params, cx);
        });
        cx.read(|cx| {
            let doc = document.read(cx);
            assert_eq!(doc.decorations().len(), 2);
            assert_eq!(doc.decorations()[0].range, 1..5);
            assert_eq!(
                doc.inlays()
                    .iter()
                    .filter(|hint| hint.text == " extension")
                    .count(),
                2
            );
        });
        document.update(cx, |doc, cx| {
            doc.edit(vec![(0..0, "prefix".into())], &[], None, cx);
            assert_eq!(doc.decorations()[0].range, 7..11);
            assert_eq!(doc.inlays().last().unwrap().offset, 12);
        });
        store.update(cx, |store, cx| store.clear_extension_decorations("one", cx));
        assert_eq!(cx.read(|cx| document.read(cx).decorations().len()), 1);
        store.update(cx, |store, cx| {
            store.decoration_said("two", "decorations.gone", json!({"type":"d1"}), cx)
        });
        cx.read(|cx| {
            assert!(document.read(cx).decorations().is_empty());
            assert_eq!(
                &**document.read(cx).inlays(),
                &[Inlay {
                    offset: 12,
                    text: " server".into()
                }]
            );
        });
    }
}
