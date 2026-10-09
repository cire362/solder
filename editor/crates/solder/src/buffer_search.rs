//! The find and replace bar above the active editor (`cmd-f`, `cmd-alt-f`).

use std::{ops::Range, sync::Arc};

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, KeyBinding, Subscription, Task, Window, actions,
    div, prelude::*, px,
};

use crate::{
    editor::{Editor, EditorEvent},
    search::{self, SearchOptions},
    theme::ActiveTheme,
    ui,
};

actions!(
    search_bar,
    [
        FindNext,
        FindPrev,
        ReplaceNext,
        ReplaceAll,
        SelectAllMatches,
        ToggleCase,
        ToggleWord,
        ToggleRegex,
        ToggleReplace,
        FocusNextField,
        Close,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let bar = Some("BufferSearchBar");
    let replace = Some("ReplaceField");
    cx.bind_keys([
        KeyBinding::new("enter", FindNext, bar),
        KeyBinding::new("shift-enter", FindPrev, bar),
        KeyBinding::new("alt-enter", SelectAllMatches, bar),
        KeyBinding::new("escape", Close, bar),
        KeyBinding::new("tab", FocusNextField, bar),
        KeyBinding::new("shift-tab", FocusNextField, bar),
        KeyBinding::new("alt-secondary-c", ToggleCase, bar),
        KeyBinding::new("alt-secondary-w", ToggleWord, bar),
        KeyBinding::new("alt-secondary-r", ToggleRegex, bar),
        KeyBinding::new("enter", ReplaceNext, replace),
        KeyBinding::new("secondary-enter", ReplaceAll, replace),
    ]);
}

/// More matches than this are not highlighted; the count says so.
const MATCH_LIMIT: usize = 20_000;

pub struct BufferSearchBar {
    query: Entity<Editor>,
    replacement: Entity<Editor>,
    options: SearchOptions,
    pub show_replace: bool,
    pub visible: bool,
    editor: Option<Entity<Editor>>,
    pub(crate) matches: Arc<Vec<Range<usize>>>,
    active: Option<usize>,
    error: Option<String>,
    search_task: Option<Task<()>>,
    /// Moves the selection to the nearest match as the query is typed.
    follow_cursor: bool,
    editor_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl BufferSearchBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| Editor::single_line("Find", cx));
        let replacement = cx.new(|cx| Editor::single_line("Replace", cx));
        let subscriptions = vec![cx.subscribe_in(&query, window, |this, _, e, _, cx| {
            if let EditorEvent::Edited = e {
                this.follow_cursor = true;
                this.research(cx);
            }
        })];
        Self {
            query,
            replacement,
            options: SearchOptions::default(),
            show_replace: false,
            visible: false,
            editor: None,
            matches: Arc::default(),
            active: None,
            error: None,
            search_task: None,
            follow_cursor: false,
            editor_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    /// Opens the bar for `editor`, prefilled with its selection.
    pub fn deploy(
        &mut self,
        editor: Entity<Editor>,
        replace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = editor.read(cx).selected_text(cx);
        self.set_editor(Some(editor), cx);
        self.visible = true;
        self.show_replace = replace;
        if let Some(text) = selected {
            self.query.update(cx, |q, cx| q.set_text(&text, true, cx));
        } else {
            let text = self.query.read(cx).text(cx);
            self.query.update(cx, |q, cx| q.set_text(&text, true, cx));
        }
        let focus = if replace && !self.query.read(cx).text(cx).is_empty() {
            self.replacement.focus_handle(cx)
        } else {
            self.query.focus_handle(cx)
        };
        window.focus(&focus);
        self.research(cx);
    }

    /// Follows the active tab. Highlights move with the bar.
    pub fn set_editor(&mut self, editor: Option<Entity<Editor>>, cx: &mut Context<Self>) {
        if self.editor == editor {
            return;
        }
        if let Some(old) = self.editor.take() {
            old.update(cx, |e, cx| e.set_search_matches(Arc::default(), None, cx));
        }
        self.editor_subscription = editor.as_ref().map(|e| {
            cx.subscribe(e, |this, _, event, cx| {
                if let EditorEvent::Edited = event {
                    this.research(cx);
                }
            })
        });
        self.editor = editor;
        self.matches = Arc::default();
        self.active = None;
        if self.visible {
            self.research(cx);
        }
    }

    fn research(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        if !self.visible {
            return;
        }
        let query = self.query.read(cx).text(cx);
        if query.is_empty() {
            self.error = None;
            self.apply_matches(Arc::default(), cx);
            return;
        }
        let re = match self.options.build(&query) {
            Ok(re) => re,
            Err(err) => {
                self.error = Some(err);
                self.apply_matches(Arc::default(), cx);
                return;
            }
        };
        self.error = None;
        let rope = editor.read(cx).rope(cx).clone();
        let version = editor.read(cx).version(cx);
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let matches = cx
                .background_executor()
                .spawn(async move { search::find_all(&rope.to_string(), &re, MATCH_LIMIT) })
                .await;
            this.update(cx, |this, cx| {
                let current = this.editor.as_ref().map(|e| e.read(cx).version(cx));
                if current == Some(version) {
                    this.apply_matches(Arc::new(matches), cx);
                }
            })
            .ok();
        }));
    }

    fn apply_matches(&mut self, matches: Arc<Vec<Range<usize>>>, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let cursor = editor.read(cx).newest_range().start;
        self.active = if matches.is_empty() {
            None
        } else {
            Some(matches.partition_point(|m| m.start < cursor) % matches.len())
        };
        self.matches = matches.clone();
        let active = self.active;
        let follow = std::mem::take(&mut self.follow_cursor);
        editor.update(cx, |e, cx| {
            e.set_search_matches(matches.clone(), active, cx);
            if follow && let Some(ix) = active {
                e.select_range(matches[ix].clone(), cx);
            }
        });
        cx.notify();
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (Some(editor), false) = (self.editor.clone(), self.matches.is_empty()) else {
            return;
        };
        let len = self.matches.len();
        let cursor = editor.read(cx).newest_range();
        // Step relative to the selection, so moving the cursor then pressing
        // Enter finds the next match from there.
        let ix = if forward {
            self.matches
                .partition_point(|m| m.start < cursor.end.max(cursor.start + 1))
                % len
        } else {
            (self.matches.partition_point(|m| m.start < cursor.start) + len - 1) % len
        };
        self.active = Some(ix);
        let range = self.matches[ix].clone();
        let matches = self.matches.clone();
        editor.update(cx, |e, cx| {
            e.set_search_matches(matches, Some(ix), cx);
            e.select_range(range, cx);
        });
        cx.notify();
    }

    pub fn find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step(true, cx);
    }

    pub fn find_prev(&mut self, _: &FindPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.step(false, cx);
    }

    fn replace_next(&mut self, _: &ReplaceNext, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(editor), Some(ix)) = (self.editor.clone(), self.active) else {
            return;
        };
        let Ok(re) = self.options.build(&self.query.read(cx).text(cx)) else {
            return;
        };
        let range = self.matches[ix].clone();
        let replacement = self.replacement.read(cx).text(cx);
        let options = self.options;
        editor.update(cx, |e, cx| {
            let matched = e.rope(cx).byte_slice(range.clone()).to_string();
            let text = search::replacement_for(&re, &matched, &replacement, options);
            e.replace_ranges(vec![(range.clone(), text.clone())], cx);
            // Park the cursor after the replacement; the research that the
            // edit triggers then selects the following match.
            let end = range.start + text.len();
            e.select_range(end..end, cx);
        });
        self.follow_cursor = true;
    }

    fn replace_all(&mut self, _: &ReplaceAll, _: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let Ok(re) = self.options.build(&self.query.read(cx).text(cx)) else {
            return;
        };
        let replacement = self.replacement.read(cx).text(cx);
        let options = self.options;
        let matches = self.matches.clone();
        editor.update(cx, |e, cx| {
            let edits = matches
                .iter()
                .map(|r| {
                    let matched = e.rope(cx).byte_slice(r.clone()).to_string();
                    (
                        r.clone(),
                        search::replacement_for(&re, &matched, &replacement, options),
                    )
                })
                .collect();
            e.replace_ranges(edits, cx);
        });
    }

    fn select_all_matches(
        &mut self,
        _: &SelectAllMatches,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let matches = self.matches.clone();
        editor.update(cx, |e, cx| e.select_ranges(&matches, cx));
        window.focus(&editor.focus_handle(cx));
    }

    fn toggle_option(&mut self, f: impl FnOnce(&mut SearchOptions), cx: &mut Context<Self>) {
        f(&mut self.options);
        self.research(cx);
        cx.notify();
    }

    fn toggle_case(&mut self, _: &ToggleCase, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_option(|o| o.case_sensitive = !o.case_sensitive, cx);
    }

    fn toggle_word(&mut self, _: &ToggleWord, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_option(|o| o.whole_word = !o.whole_word, cx);
    }

    fn toggle_regex(&mut self, _: &ToggleRegex, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_option(|o| o.regex = !o.regex, cx);
    }

    fn toggle_replace(&mut self, _: &ToggleReplace, window: &mut Window, cx: &mut Context<Self>) {
        self.show_replace = !self.show_replace;
        if self.show_replace {
            window.focus(&self.replacement.focus_handle(cx));
        } else {
            window.focus(&self.query.focus_handle(cx));
        }
        cx.notify();
    }

    fn focus_next_field(
        &mut self,
        _: &FocusNextField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let in_query = self.query.focus_handle(cx).is_focused(window);
        if in_query {
            self.show_replace = true;
            window.focus(&self.replacement.focus_handle(cx));
        } else {
            window.focus(&self.query.focus_handle(cx));
        }
        cx.notify();
    }

    pub fn close(&mut self, _: &Close, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = false;
        self.search_task = None;
        if let Some(editor) = self.editor.clone() {
            editor.update(cx, |e, cx| e.set_search_matches(Arc::default(), None, cx));
            window.focus(&editor.focus_handle(cx));
        }
        self.matches = Arc::default();
        self.active = None;
        cx.notify();
    }

    fn status(&self) -> String {
        if let Some(err) = &self.error {
            return err.clone();
        }
        match (self.matches.len(), self.active) {
            (0, _) => "No results".into(),
            (n, Some(i)) if n >= MATCH_LIMIT => format!("{} of {}+", i + 1, n),
            (n, Some(i)) => format!("{} of {}", i + 1, n),
            (n, None) => format!("{n} results"),
        }
    }
}

impl Focusable for BufferSearchBar {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl Render for BufferSearchBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let options = self.options;
        let query_focused = self.query.focus_handle(cx).is_focused(window);
        let replace_focused = self.replacement.focus_handle(cx).is_focused(window);
        let has_query = !self.query.read(cx).text(cx).is_empty();
        let entity = cx.entity();
        let on = move |f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let entity = entity.clone();
            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| f(this, window, cx));
            }
        };

        div()
            .key_context("BufferSearchBar")
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_prev))
            .on_action(cx.listener(Self::replace_next))
            .on_action(cx.listener(Self::replace_all))
            .on_action(cx.listener(Self::select_all_matches))
            .on_action(cx.listener(Self::toggle_case))
            .on_action(cx.listener(Self::toggle_word))
            .on_action(cx.listener(Self::toggle_regex))
            .on_action(cx.listener(Self::toggle_replace))
            .on_action(cx.listener(Self::focus_next_field))
            .on_action(cx.listener(Self::close))
            .flex_none()
            .flex()
            .flex_col()
            .gap_1p5()
            .px_3()
            .py_2()
            .border_b(theme.shape.border)
            .border_color(theme.line)
            .bg(theme.bg_sunken)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(ui::toggle(
                        "toggle-replace",
                        if self.show_replace { "▾" } else { "▸" },
                        "Toggle replace",
                        false,
                        &theme,
                        on(|this, window, cx| this.toggle_replace(&ToggleReplace, window, cx)),
                    ))
                    .child(ui::text_field(self.query.clone(), query_focused, &theme))
                    .child(ui::toggle(
                        "case",
                        "Aa",
                        "Match case",
                        options.case_sensitive,
                        &theme,
                        on(|this, window, cx| this.toggle_case(&ToggleCase, window, cx)),
                    ))
                    .child(ui::toggle(
                        "word",
                        "ab",
                        "Whole word",
                        options.whole_word,
                        &theme,
                        on(|this, window, cx| this.toggle_word(&ToggleWord, window, cx)),
                    ))
                    .child(ui::toggle(
                        "regex",
                        ".*",
                        "Regular expression",
                        options.regex,
                        &theme,
                        on(|this, window, cx| this.toggle_regex(&ToggleRegex, window, cx)),
                    ))
                    .child(
                        div()
                            .w(px(96.))
                            .flex_none()
                            .text_size(crate::theme::text(11.5))
                            .truncate()
                            .text_color(if self.error.is_some() {
                                theme.error
                            } else {
                                theme.fg_subtle
                            })
                            .when(has_query, |d| d.child(self.status())),
                    )
                    .child(ui::toggle(
                        "prev",
                        "↑",
                        "Previous match",
                        false,
                        &theme,
                        on(|this, window, cx| this.find_prev(&FindPrev, window, cx)),
                    ))
                    .child(ui::toggle(
                        "next",
                        "↓",
                        "Next match",
                        false,
                        &theme,
                        on(|this, window, cx| this.find_next(&FindNext, window, cx)),
                    ))
                    .child(ui::toggle(
                        "close",
                        "×",
                        "Close",
                        false,
                        &theme,
                        on(|this, window, cx| this.close(&Close, window, cx)),
                    )),
            )
            .when(self.show_replace, |d| {
                d.child(
                    div()
                        .key_context("ReplaceField")
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .pl(px(29.5))
                        .child(ui::text_field(
                            self.replacement.clone(),
                            replace_focused,
                            &theme,
                        ))
                        .child(ui::button(
                            "replace-one",
                            "Replace",
                            false,
                            &theme,
                            on(|this, window, cx| this.replace_next(&ReplaceNext, window, cx)),
                        ))
                        .child(ui::button(
                            "replace-all",
                            "Replace all",
                            false,
                            &theme,
                            on(|this, window, cx| this.replace_all(&ReplaceAll, window, cx)),
                        ))
                        .child(div().w(px(76.)).flex_none()),
                )
            })
    }
}
