use std::{path::PathBuf, sync::Arc};

use gpui::{
    AnyElement, Context, DismissEvent, SharedString, Task, WeakEntity, Window, div, prelude::*,
};

use crate::{
    fuzzy,
    picker::{Picker, PickerDelegate, highlighted_text},
    theme::{ActiveTheme, UI_FONT_SIZE},
    workspace::Workspace,
};

const MAX_RESULTS: usize = 200;

pub struct FileFinder {
    workspace: WeakEntity<Workspace>,
    root: PathBuf,
    files: Arc<Vec<Arc<str>>>,
    /// Recently opened files, most recent first, shown for an empty query.
    recent: Vec<Arc<str>>,
    query: String,
    matches: Vec<(Arc<str>, Vec<u32>)>,
    selected: usize,
}

impl FileFinder {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        root: PathBuf,
        files: Arc<Vec<Arc<str>>>,
        recent: Vec<Arc<str>>,
    ) -> Self {
        Self {
            workspace,
            root,
            files,
            recent,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
        }
    }
}

impl PickerDelegate for FileFinder {
    fn placeholder(&self) -> SharedString {
        "Go to file".into()
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
        self.query = query.clone();
        if query.is_empty() {
            let mut seen = std::collections::HashSet::new();
            self.matches = self
                .recent
                .iter()
                .chain(self.files.iter())
                .filter(|f| seen.insert(Arc::clone(f)))
                .take(MAX_RESULTS)
                .map(|f| (f.clone(), Vec::new()))
                .collect();
            self.selected = 0;
            return Task::ready(());
        }
        let files = self.files.clone();
        cx.spawn(async move |picker, cx| {
            let matches = cx
                .background_executor()
                .spawn(async move {
                    fuzzy::fuzzy_match(files.iter().map(|f| &**f), &query, MAX_RESULTS, true)
                        .into_iter()
                        .map(|m| {
                            let path = files[m.index].clone();
                            let positions = fuzzy::positions(&path, &query, true);
                            (path, positions)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            picker
                .update(cx, |picker, cx| {
                    picker.delegate.matches = matches;
                    picker.delegate.selected = 0;
                    picker.matches_updated(cx);
                })
                .ok();
        })
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((path, _)) = self.matches.get(self.selected) else {
            return;
        };
        let path = self.root.join(&**path);
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |w, cx| w.open_path(path, None, window, cx))
            .ok();
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (path, positions) = &self.matches[ix];
        let (dir, name) = match path.rfind('/') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", &path[..]),
        };
        let name_start = path[..path.len() - name.len()].chars().count() as u32;
        let name_positions: Vec<u32> = positions
            .iter()
            .filter(|p| **p >= name_start)
            .map(|p| p - name_start)
            .collect();
        let dir_positions: Vec<u32> = positions
            .iter()
            .copied()
            .filter(|p| *p < name_start)
            .collect();
        div()
            .flex()
            .items_baseline()
            .gap_2()
            .min_w_0()
            .text_size(UI_FONT_SIZE)
            .child(
                div()
                    .flex_none()
                    .text_color(theme.fg)
                    .child(highlighted_text(
                        name,
                        &name_positions,
                        theme.fg,
                        theme.accent,
                    )),
            )
            .child(
                div()
                    .truncate()
                    .text_size(gpui::px(11.5))
                    .text_color(theme.fg_subtle)
                    .child(highlighted_text(
                        dir,
                        &dir_positions,
                        theme.fg_subtle,
                        theme.accent,
                    )),
            )
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        if self.files.is_empty() {
            "Indexing files...".into()
        } else {
            format!("No file matches \u{201c}{}\u{201d}", self.query).into()
        }
    }
}
