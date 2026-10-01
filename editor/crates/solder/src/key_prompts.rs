//! Small prompts for Redis keys: when a key expires, its new name, and a
//! new key's name and type.

use gpui::{
    AnyElement, Context, DismissEvent, SharedString, Task, WeakEntity, Window, div, prelude::*, px,
};

use crate::{
    picker::{Picker, PickerDelegate},
    results::{KeyAction, ResultsView, redis_arg},
    theme::{ActiveTheme, UI_FONT_SIZE},
};

/// A label, the command it runs, and the query to show afterwards.
type Choice = (String, String, Option<String>);

/// Expire (seconds, or never) or rename the key a results view shows.
pub struct KeyPrompt {
    results: WeakEntity<ResultsView>,
    action: KeyAction,
    key: String,
    choices: Vec<Choice>,
    selected: usize,
    query: String,
}

impl KeyPrompt {
    pub fn new(results: WeakEntity<ResultsView>, action: KeyAction, key: String) -> Self {
        Self {
            results,
            action,
            key,
            choices: Vec::new(),
            selected: 0,
            query: String::new(),
        }
    }

    /// What the view shows after renaming: the same reading command for
    /// the new name.
    fn reread(query: &str, from: &str, to: &str) -> String {
        query.replacen(&redis_arg(from), &redis_arg(to), 1)
    }
}

impl PickerDelegate for KeyPrompt {
    fn placeholder(&self) -> SharedString {
        match self.action {
            KeyAction::Expire => format!("Seconds until {} expires", self.key),
            KeyAction::Rename => format!("New name for {}", self.key),
        }
        .into()
    }

    fn match_count(&self) -> usize {
        self.choices.len()
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
        self.query = query.trim().to_string();
        let key = redis_arg(&self.key);
        let shown = self
            .results
            .read_with(cx, |r, _| r.query.to_string())
            .unwrap_or_default();
        self.choices = match self.action {
            KeyAction::Expire => {
                let mut choices = Vec::new();
                if let Ok(seconds) = self.query.parse::<u64>() {
                    choices.push((
                        format!("Expire in {seconds} s"),
                        format!("EXPIRE {key} {seconds}"),
                        Some(shown.clone()),
                    ));
                }
                choices.push(("Never expire".into(), format!("PERSIST {key}"), Some(shown)));
                choices
            }
            KeyAction::Rename if !self.query.is_empty() && self.query != self.key => vec![(
                format!("Rename to {}", self.query),
                format!("RENAME {key} {}", redis_arg(&self.query)),
                Some(Self::reread(&shown, &self.key, &self.query)),
            )],
            KeyAction::Rename => Vec::new(),
        };
        self.selected = 0;
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((_, command, then)) = self.choices.get(self.selected).cloned() else {
            return;
        };
        self.results
            .update(cx, |r, cx| r.key_command(command, then, cx))
            .ok();
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        div()
            .text_size(UI_FONT_SIZE)
            .text_color(cx.theme().fg)
            .child(self.choices[ix].0.clone())
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        "Type a new name".into()
    }

    fn width(&self) -> gpui::Pixels {
        px(420.)
    }
}

const TYPES: [&str; 5] = ["string", "hash", "list", "set", "zset"];

/// Shows the Results tab once a new key opens there.
type OnOpen = Box<dyn Fn(&mut Window, &mut gpui::App)>;

/// A new key: type its name, pick its type.
pub struct NewKeyPrompt {
    results: WeakEntity<ResultsView>,
    connection: SharedString,
    name: String,
    selected: usize,
    on_open: OnOpen,
}

impl NewKeyPrompt {
    /// `on_open` shows the Results tab once the key opens there.
    pub fn new(
        results: WeakEntity<ResultsView>,
        connection: SharedString,
        on_open: impl Fn(&mut Window, &mut gpui::App) + 'static,
    ) -> Self {
        Self {
            results,
            connection,
            name: String::new(),
            selected: 0,
            on_open: Box::new(on_open),
        }
    }
}

impl PickerDelegate for NewKeyPrompt {
    fn placeholder(&self) -> SharedString {
        "Name of the new key".into()
    }

    fn match_count(&self) -> usize {
        if self.name.is_empty() { 0 } else { TYPES.len() }
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
        self.name = query.trim().to_string();
        Task::ready(())
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if self.name.is_empty() {
            return;
        }
        let (kind, name, connection) = (
            TYPES[self.selected],
            self.name.clone(),
            self.connection.clone(),
        );
        self.results
            .update(cx, |r, cx| r.new_key(connection, kind, &name, cx))
            .ok();
        (self.on_open)(window, cx);
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        div()
            .text_size(UI_FONT_SIZE)
            .text_color(cx.theme().fg)
            .child(format!("New {} {}", TYPES[ix], self.name))
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        "Type the key's name, then pick its type".into()
    }

    fn width(&self) -> gpui::Pixels {
        px(420.)
    }
}
