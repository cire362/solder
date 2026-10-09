//! The Response tab in the bottom dock: the last HTTP request's status,
//! timing, headers and body (JSON pretty-printed). Lines are virtualized,
//! so a large body costs only what is on screen.

use std::sync::Arc;

use gpui::{
    App, ClipboardItem, Context, FocusHandle, Focusable, Hsla, Pixels, SharedString, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use rest::{Request, Response};

use crate::{
    settings::Settings,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

const LINE: Pixels = px(20.);
/// Characters drawn per line; the rest is a copy away.
const LINE_CHARS: usize = 400;

pub enum ResponseState {
    Empty,
    Sending,
    Done(Box<Response>, Arc<Vec<String>>),
    Failed(SharedString),
}

pub struct ResponseView {
    pub request: Option<Request>,
    pub state: ResponseState,
    pub show_headers: bool,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    task: Option<Task<()>>,
}

impl ResponseView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            request: None,
            state: ResponseState::Empty,
            show_headers: false,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            task: None,
        }
    }

    pub fn send(&mut self, request: Request, cx: &mut Context<Self>) {
        self.request = Some(request.clone());
        self.state = ResponseState::Sending;
        let sending = rest::send(request);
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = sending.await;
            this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(response) => {
                        let lines = response.text().lines().map(str::to_string).collect();
                        ResponseState::Done(Box::new(response), Arc::new(lines))
                    }
                    Err(e) => ResponseState::Failed(e.into()),
                };
                this.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Lines shown: the body, or the headers.
    fn lines(&self) -> Arc<Vec<String>> {
        match &self.state {
            ResponseState::Done(response, body) if self.show_headers => Arc::new(
                std::iter::once(format!(
                    "{} {} {}",
                    response.version, response.status, response.reason
                ))
                .chain(response.headers.iter().map(|(n, v)| format!("{n}: {v}")))
                .collect(),
            ),
            ResponseState::Done(_, body) => body.clone(),
            _ => Arc::new(Vec::new()),
        }
    }

    fn status_color(status: u16, theme: &Theme) -> Hsla {
        match status {
            200..=299 => theme.git_added,
            300..=399 => theme.accent,
            400..=499 => theme.warning,
            _ => theme.error,
        }
    }

    fn render_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let (status, detail): (Option<(String, Hsla)>, String) = match &self.state {
            ResponseState::Empty => (None, String::new()),
            ResponseState::Sending => (None, "Sending...".into()),
            ResponseState::Failed(e) => (Some(("Failed".into(), theme.error)), e.to_string()),
            ResponseState::Done(r, _) => {
                let size = r.body.len();
                let size = if size >= 1024 * 1024 {
                    format!("{:.1} MB", size as f64 / 1048576.)
                } else if size >= 1024 {
                    format!("{:.1} KB", size as f64 / 1024.)
                } else {
                    format!("{size} B")
                };
                (
                    Some((
                        format!("{} {}", r.status, r.reason),
                        Self::status_color(r.status, theme),
                    )),
                    format!(
                        "{:.0} ms · {size}{}",
                        r.elapsed.as_secs_f64() * 1000.,
                        if r.truncated { " (cut at 10 MB)" } else { "" }
                    ),
                )
            }
        };
        let request = self
            .request
            .as_ref()
            .map(|r| format!("{} {}", r.method, r.url))
            .unwrap_or_default();
        div()
            .flex_none()
            .h(px(34.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b(theme.shape.border)
            .border_color(theme.line)
            .text_size(UI_FONT_SIZE)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(crate::theme::CODE_FONT)
                    .text_color(theme.fg_subtle)
                    .child(request),
            )
            .children(status.map(|(text, color)| {
                div()
                    .flex_none()
                    .px_1p5()
                    .rounded(theme.shape.token)
                    .bg(theme.bg_elev)
                    .text_color(color)
                    .child(text)
            }))
            .child(
                div()
                    .flex_none()
                    .max_w(px(520.))
                    .truncate()
                    .text_color(theme.fg_muted)
                    .child(detail),
            )
            .when(matches!(self.state, ResponseState::Done(..)), |d| {
                d.child(ui::toggle(
                    "response-body",
                    "body",
                    "",
                    !self.show_headers,
                    theme,
                    {
                        let view = view.clone();
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.show_headers = false;
                                cx.notify();
                            })
                        }
                    },
                ))
                .child(ui::toggle(
                    "response-headers",
                    "headers",
                    "",
                    self.show_headers,
                    theme,
                    {
                        let view = view.clone();
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.show_headers = true;
                                cx.notify();
                            })
                        }
                    },
                ))
                .child(ui::button("response-copy", "Copy", false, theme, {
                    let view = view.clone();
                    move |_, _, cx| {
                        let text = view.read(cx).lines().join("\n");
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }))
            })
    }
}

impl Focusable for ResponseView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ResponseView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let settings = Settings::get(cx).clone();
        let lines = self.lines();
        let count = lines.len();
        div()
            .id("response")
            .key_context("Response")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .child(self.render_bar(&theme, cx))
            .child(
                uniform_list("response-lines", count, move |range, _, cx| {
                    let theme = cx.theme().clone();
                    range
                        .map(|i| {
                            let text: String = lines[i].chars().take(LINE_CHARS).collect();
                            div()
                                .h(LINE)
                                .px_3()
                                .flex()
                                .items_center()
                                .text_color(theme.fg)
                                .child(div().truncate().child(text))
                        })
                        .collect()
                })
                .track_scroll(self.scroll.clone())
                .flex_1()
                .font_family(settings.buffer_font_family.clone())
                .text_size(settings.buffer_font_size() - px(1.)),
            )
    }
}
