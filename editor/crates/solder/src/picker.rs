//! A query field over a filtered list. The command palette, file finder and
//! go-to-line all build on this; only the delegate differs.

use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, MouseButton, Pixels, ScrollStrategy, SharedString, Subscription, Task,
    UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::{
    editor::{Editor, EditorEvent},
    theme::{ActiveTheme, UI_FONT_SIZE},
};

actions!(
    picker,
    [
        SelectPrev,
        SelectNext,
        SelectFirst,
        SelectLast,
        Confirm,
        Dismiss
    ]
);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("Picker");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, ctx),
        KeyBinding::new("ctrl-p", SelectPrev, ctx),
        KeyBinding::new("down", SelectNext, ctx),
        KeyBinding::new("ctrl-n", SelectNext, ctx),
        KeyBinding::new("pageup", SelectFirst, ctx),
        KeyBinding::new("pagedown", SelectLast, ctx),
        KeyBinding::new("enter", Confirm, ctx),
        KeyBinding::new("escape", Dismiss, ctx),
    ]);
}

pub const ROW_HEIGHT: Pixels = px(30.);
const MAX_VISIBLE_ROWS: usize = 12;

pub trait PickerDelegate: Sized + 'static {
    fn placeholder(&self) -> SharedString;
    fn match_count(&self) -> usize;
    fn selected_index(&self) -> usize;
    fn set_selected_index(&mut self, ix: usize, cx: &mut Context<Picker<Self>>);
    /// Recomputes matches for `query`. Long work belongs in the returned task,
    /// which is dropped (cancelled) when the query changes again.
    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()>;
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>);
    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement;

    fn empty_text(&self) -> SharedString {
        "No matches".into()
    }

    fn width(&self) -> Pixels {
        px(600.)
    }
}

pub struct Picker<D: PickerDelegate> {
    pub delegate: D,
    query: Entity<Editor>,
    scroll: UniformListScrollHandle,
    pending: Option<Task<()>>,
    _subscription: Subscription,
}

impl<D: PickerDelegate> EventEmitter<DismissEvent> for Picker<D> {}

impl<D: PickerDelegate> Picker<D> {
    pub fn new(delegate: D, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| Editor::single_line(delegate.placeholder(), cx));
        let subscription = cx.subscribe_in(&query, window, |this, _, event, window, cx| {
            if let EditorEvent::Edited = event {
                this.refresh(window, cx);
            }
        });
        let mut picker = Self {
            delegate,
            query,
            scroll: UniformListScrollHandle::new(),
            pending: None,
            _subscription: subscription,
        };
        picker.refresh(window, cx);
        picker
    }

    pub fn query(&self, cx: &App) -> String {
        self.query.read(cx).text(cx)
    }

    /// Prefills the query, selected so typing replaces it.
    pub fn set_query(&mut self, text: &str, cx: &mut Context<Self>) {
        self.query.update(cx, |e, cx| e.set_text(text, true, cx));
    }

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query(cx);
        self.pending = Some(self.delegate.update_matches(query, window, cx));
        cx.notify();
    }

    /// Called by delegates after matches arrive asynchronously.
    pub fn matches_updated(&mut self, cx: &mut Context<Self>) {
        let ix = self.delegate.selected_index();
        self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        cx.notify();
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.delegate.match_count() == 0 {
            return;
        }
        self.delegate.set_selected_index(ix, cx);
        self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        cx.notify();
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.match_count();
        if count > 0 {
            let ix = self.delegate.selected_index();
            self.select(if ix == 0 { count - 1 } else { ix - 1 }, cx);
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.match_count();
        if count > 0 {
            self.select((self.delegate.selected_index() + 1) % count, cx);
        }
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.select(0, cx);
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.match_count();
        self.select(count.saturating_sub(1), cx);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.delegate.confirm(window, cx);
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl<D: PickerDelegate> Focusable for Picker<D> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl<D: PickerDelegate> Render for Picker<D> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.delegate.match_count();
        let row = crate::theme::row(ROW_HEIGHT, cx);
        let list_height = row * count.clamp(1, MAX_VISIBLE_ROWS) as f32;
        div()
            .key_context("Picker")
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::dismiss))
            .w(self.delegate.width())
            .flex()
            .flex_col()
            .bg(theme.bg_elev)
            .border(theme.shape.border)
            .border_color(theme.line)
            .rounded(theme.shape.panel)
            .shadow_lg()
            .overflow_hidden()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b(theme.shape.border)
                    .border_color(theme.line)
                    .child(self.query.clone()),
            )
            .child(if count == 0 {
                div()
                    .h(row + px(8.))
                    .px_4()
                    .flex()
                    .items_center()
                    .text_size(UI_FONT_SIZE)
                    .text_color(theme.fg_subtle)
                    .child(self.delegate.empty_text())
                    .into_any_element()
            } else {
                uniform_list(
                    "picker-matches",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                        let selected = this.delegate.selected_index();
                        range
                            .map(|ix| {
                                let content =
                                    this.delegate.render_match(ix, ix == selected, window, cx);
                                div()
                                    .id(ix)
                                    .h(row)
                                    .mx_1p5()
                                    .px_2p5()
                                    .flex()
                                    .items_center()
                                    .rounded(theme.shape.control)
                                    .when(ix == selected, |d| d.bg(cx.theme().accent_soft))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, window, cx| {
                                            this.select(ix, cx);
                                            this.delegate.confirm(window, cx);
                                        }),
                                    )
                                    .child(content)
                            })
                            .collect()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .h(list_height)
                .py_1p5()
                .into_any_element()
            })
    }
}

/// Characters of `text` at `positions` (char indices, sorted) drawn in the
/// accent color. Used by delegates to show why an item matched.
pub fn highlighted_text(
    text: &str,
    positions: &[u32],
    base: gpui::Hsla,
    accent: gpui::Hsla,
) -> gpui::StyledText {
    use gpui::{HighlightStyle, StyledText};
    let mut ranges = Vec::new();
    let mut byte = 0;
    let mut next = positions.iter().peekable();
    for (i, c) in text.chars().enumerate() {
        if next.peek().is_some_and(|p| **p as usize == i) {
            next.next();
            ranges.push((
                byte..byte + c.len_utf8(),
                HighlightStyle {
                    color: Some(accent),
                    ..Default::default()
                },
            ));
        }
        byte += c.len_utf8();
    }
    let _ = base;
    StyledText::new(SharedString::from(text.to_owned())).with_highlights(ranges)
}

/// Keystrokes of a binding in the platform's usual notation.
pub fn format_binding(binding: &KeyBinding) -> String {
    let mac = cfg!(target_os = "macos");
    binding
        .keystrokes()
        .iter()
        .map(|k| {
            let mut out = String::new();
            let m = k.modifiers();
            if mac {
                if m.control {
                    out.push('⌃');
                }
                if m.alt {
                    out.push('⌥');
                }
                if m.shift {
                    out.push('⇧');
                }
                if m.platform {
                    out.push('⌘');
                }
            } else {
                for (on, name) in [
                    (m.control, "Ctrl+"),
                    (m.alt, "Alt+"),
                    (m.shift, "Shift+"),
                    (m.platform, "Super+"),
                ] {
                    if on {
                        out.push_str(name);
                    }
                }
            }
            let key = k.key();
            let key = match key {
                "enter" => "↵".to_string(),
                "escape" => "Esc".to_string(),
                "backspace" => "⌫".to_string(),
                "up" => "↑".to_string(),
                "down" => "↓".to_string(),
                "left" => "←".to_string(),
                "right" => "→".to_string(),
                "tab" => "⇥".to_string(),
                k if k.len() == 1 => k.to_uppercase(),
                k => k[..1].to_uppercase() + &k[1..],
            };
            out.push_str(&key);
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}
