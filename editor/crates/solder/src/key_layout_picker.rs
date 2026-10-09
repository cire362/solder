//! Choose or copy a set of keys. Personal overrides stay in keymap.json.

use gpui::{
    Action, AnyElement, Context, DismissEvent, SharedString, Task, Window, div, prelude::*,
};
use serde::Deserialize;

use crate::{
    fuzzy, key_layout,
    picker::{Picker, PickerDelegate, highlighted_text},
    theme::{ActiveTheme, UI_FONT_SIZE},
};

/// With no name, opens the list. A name selects a built-in or user set.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwitchKeyLayout {
    pub name: Option<String>,
}

// Only deserialization is needed for this action's arguments; implementing
// it here avoids adding a schema dependency just for one string.
impl Action for SwitchKeyLayout {
    fn name(&self) -> &'static str {
        Self::name_for_type()
    }
    fn name_for_type() -> &'static str {
        "workspace::SwitchKeyLayout"
    }
    fn boxed_clone(&self) -> Box<dyn Action> {
        Box::new(self.clone())
    }
    fn partial_eq(&self, other: &dyn Action) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
    fn build(value: serde_json::Value) -> gpui::Result<Box<dyn Action>> {
        Ok(Box::new(serde_json::from_value::<Self>(value)?))
    }
}
gpui::register_action!(SwitchKeyLayout);

pub struct KeyLayoutPicker {
    save: bool,
    loaded: bool,
    names: Vec<String>,
    query: String,
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
}

impl KeyLayoutPicker {
    pub fn new(save: bool) -> Self {
        Self {
            save,
            loaded: false,
            names: Vec::new(),
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
        }
    }

    pub fn load(window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let dir = key_layout::folder(cx);
        cx.spawn_in(window, async move |picker, cx| {
            let names = cx
                .background_executor()
                .spawn(async move { key_layout::names(&dir) })
                .await;
            picker
                .update_in(cx, |picker, window, cx| {
                    picker.delegate.names = key_layout::BUILT_INS
                        .into_iter()
                        .map(str::to_owned)
                        .chain(names)
                        .collect();
                    picker.delegate.loaded = true;
                    picker.refresh(window, cx);
                })
                .ok();
        })
        .detach();
    }

    fn can_save(&self) -> bool {
        self.loaded && key_layout::valid_name(&self.query) && !self.names.contains(&self.query)
    }
}

impl PickerDelegate for KeyLayoutPicker {
    fn placeholder(&self) -> SharedString {
        if self.save {
            "Name for this key layout"
        } else {
            "Switch key layout"
        }
        .into()
    }
    fn match_count(&self) -> usize {
        if self.save {
            usize::from(self.can_save())
        } else {
            self.matches.len()
        }
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
        self.query = query.trim().to_string();
        self.matches = if self.query.is_empty() {
            (0..self.names.len()).map(|i| (i, Vec::new())).collect()
        } else {
            fuzzy::fuzzy_match(
                self.names.iter().map(String::as_str),
                &self.query,
                100,
                false,
            )
            .into_iter()
            .map(|m| {
                let positions = fuzzy::positions(&self.names[m.index], &self.query, false);
                (m.index, positions)
            })
            .collect()
        };
        self.selected = 0;
        Task::ready(())
    }
    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        if self.save {
            if !self.can_save() {
                return;
            }
            key_layout::save_as(self.query.clone(), cx);
        } else {
            let Some((index, _)) = self.matches.get(self.selected) else {
                return;
            };
            let name = &self.names[*index];
            key_layout::choose(name.clone(), cx);
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
        let label = if self.save {
            div()
                .debug_selector(|| "key-layout-save".into())
                .child(format!("Save as {}", self.query))
        } else {
            let (index, positions) = &self.matches[ix];
            let name = &self.names[*index];
            let current = key_layout::active(cx);
            div()
                .debug_selector(|| format!("key-layout-choice-{ix}"))
                .child(highlighted_text(name, positions, theme.fg, theme.accent))
                .when(name == current, |d| d.child(" (in use)"))
        };
        label
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .into_any_element()
    }
    fn empty_text(&self) -> SharedString {
        if !self.loaded {
            "Loading key layouts"
        } else if self.save && self.query.is_empty() {
            "Type a name"
        } else if self.save && self.names.contains(&self.query) {
            "A key layout with this name exists"
        } else if self.save {
            "Use letters, numbers, spaces, - or _"
        } else {
            "No matches"
        }
        .into()
    }
}
