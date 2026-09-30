//! Small shared widgets. Shape rule from the site: controls are 8px, inline
//! tokens 6px.

use gpui::{
    AnyView, ClickEvent, Div, ElementId, Hsla, SharedString, Stateful, Window, div, prelude::*, px,
};

use crate::theme::{Theme, UI_FONT_SIZE};

/// A bordered box around a single-line editor.
pub fn text_field(editor: impl Into<AnyView>, focused: bool, theme: &Theme) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .h(px(28.))
        .px_2()
        .flex()
        .items_center()
        .rounded(px(8.))
        .border_1()
        .border_color(if focused { theme.accent } else { theme.line })
        .bg(theme.bg)
        .child(editor.into())
}

/// A compact toggle, e.g. case-sensitive search.
pub fn toggle(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    tooltip_hint: &'static str,
    active: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    let _ = tooltip_hint;
    div()
        .id(id)
        .flex_none()
        .h(px(22.))
        .min_w(px(24.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .text_size(px(11.5))
        .font_family(crate::theme::CODE_FONT)
        .text_color(if active {
            theme.accent
        } else {
            theme.fg_subtle
        })
        .when(active, |d| d.bg(theme.accent_soft))
        .hover(|d| d.text_color(theme.fg))
        .child(label.into())
        .on_click(on_click)
}

/// A text button. `primary` uses the accent fill.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    let (bg, fg): (Hsla, Hsla) = if primary {
        (theme.accent, theme.accent_fg)
    } else {
        (theme.bg_sunken, theme.fg_muted)
    };
    div()
        .id(id)
        .flex_none()
        .h(px(26.))
        .px_2p5()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .border_1()
        .border_color(theme.line)
        .bg(bg)
        .text_size(UI_FONT_SIZE)
        .text_color(fg)
        .hover(|d| d.opacity(0.9))
        .active(|d| d.opacity(0.8))
        .child(label.into())
        .on_click(on_click)
}
