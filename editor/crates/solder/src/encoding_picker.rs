//! Choose what a file is written in: to read it again as that, or to
//! save it as that from now on.

use gpui::{
    AnyElement, Context, DismissEvent, Entity, SharedString, Task, Window, div, prelude::*,
};
use text::encoding::Encoding;

use crate::{
    document::Document,
    fuzzy,
    picker::{Picker, PickerDelegate, highlighted_text},
    theme::{ActiveTheme, UI_FONT_SIZE},
};

pub struct EncodingPicker {
    document: Entity<Document>,
    /// Read the file again in the one chosen, where it is not to be
    /// saved in it.
    reopen: bool,
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
}

impl EncodingPicker {
    pub fn new(document: Entity<Document>, reopen: bool) -> Self {
        Self {
            document,
            reopen,
            matches: Vec::new(),
            selected: 0,
        }
    }
}

impl PickerDelegate for EncodingPicker {
    fn placeholder(&self) -> SharedString {
        match self.reopen {
            true => "Read the file again as written in",
            false => "Save the file as written in",
        }
        .into()
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
        _: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let names = Encoding::ALL.map(Encoding::name);
        let query = query.trim();
        self.matches = match query.is_empty() {
            true => (0..names.len()).map(|at| (at, Vec::new())).collect(),
            false => fuzzy::fuzzy_match(names.iter().copied(), query, 100, false)
                .into_iter()
                .map(|found| {
                    let positions = fuzzy::positions(names[found.index], query, false);
                    (found.index, positions)
                })
                .collect(),
        };
        self.selected = 0;
        Task::ready(())
    }
    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((at, _)) = self.matches.get(self.selected) else {
            return;
        };
        let encoding = Encoding::ALL[*at];
        let document = self.document.clone();
        match self.reopen {
            // What is on disk, read as the one chosen. Changes not saved
            // would be lost with it, so a file that has some is left.
            true => {
                let path = document.read(cx).path().map(std::path::Path::to_path_buf);
                let clean = !document.read(cx).is_dirty();
                if let Some(path) = path.filter(|_| clean) {
                    cx.spawn(async move |_, cx| {
                        let read = cx
                            .background_executor()
                            .spawn(async move { std::fs::read(&path) })
                            .await;
                        if let Ok(bytes) = read {
                            document
                                .update(cx, |document, cx| document.reopen(&bytes, encoding, cx))
                                .ok();
                        }
                    })
                    .detach();
                }
            }
            false => document.update(cx, |document, cx| {
                document.set_encoding(encoding, cx);
                document.save(cx).detach();
            }),
        }
        cx.emit(DismissEvent);
    }
    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (at, positions) = &self.matches[ix];
        let encoding = Encoding::ALL[*at];
        let current = self.document.read(cx).encoding() == encoding;
        div()
            .debug_selector(move || format!("encoding-{ix}"))
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .child(highlighted_text(
                encoding.name(),
                positions,
                theme.fg,
                theme.accent,
            ))
            .when(current, |d| d.child(" (as it is now)"))
            .into_any_element()
    }
    fn empty_text(&self) -> SharedString {
        "No encoding of that name".into()
    }
}
