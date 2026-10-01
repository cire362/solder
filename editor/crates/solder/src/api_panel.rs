//! The API tab: the routes the project serves, found in its code, and the
//! operations of its OpenAPI files. A route becomes a request in the
//! project's requests file, ready to send with `cmd-enter`, or is sent right
//! away.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Task,
    Window, actions, div, prelude::*, px, uniform_list,
};

use crate::{
    editor::{Editor, EditorEvent},
    fuzzy,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(api, [RefreshRoutes, ImportOpenApi, OpenRequests]);

/// One route or operation, with its request already written.
#[derive(Clone, Debug)]
pub struct RouteRow {
    pub method: String,
    pub path: String,
    /// `src/app/api/users/route.ts:3` or the spec's file name.
    pub source: String,
    /// A `.http` request; code routes use `{{baseUrl}}`.
    pub request: String,
    /// The framework that serves it; `None` for OpenAPI operations.
    pub framework: Option<&'static str>,
}

pub enum ApiEvent {
    /// Add this request to the requests file and open it there.
    Open(String),
    /// Send this request now.
    Send(String),
}

impl EventEmitter<ApiEvent> for ApiPanel {}

const ROW_HEIGHT: gpui::Pixels = px(28.);

pub struct ApiPanel {
    root: PathBuf,
    pub routes: Vec<RouteRow>,
    pub loaded: bool,
    matches: Vec<usize>,
    filter: Entity<Editor>,
    task: Option<Task<()>>,
    focus: FocusHandle,
}

impl ApiPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| Editor::single_line("Filter routes", cx));
        cx.subscribe(&filter, |this, _, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::Edited) {
                this.refilter(cx);
            }
        })
        .detach();
        Self {
            root,
            routes: Vec::new(),
            loaded: false,
            matches: Vec::new(),
            filter,
            task: None,
            focus: cx.focus_handle(),
        }
    }

    /// Reads routes the first time the tab is shown.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        if !self.loaded && self.task.is_none() {
            self.reload(cx);
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let detect = cx.background_executor().spawn(async move {
            let mut rows: Vec<RouteRow> = rest::routes::detect(&root)
                .iter()
                .map(|r| RouteRow {
                    method: r.method.clone(),
                    path: r.path.clone(),
                    source: format!("{}:{}", r.file.display(), r.line + 1),
                    request: rest::routes::to_http(r),
                    framework: Some(r.framework),
                })
                .collect();
            for spec_path in rest::openapi::find_specs(&root) {
                let Ok(text) = std::fs::read_to_string(&spec_path) else {
                    continue;
                };
                let Ok(spec) = rest::openapi::parse(&text) else {
                    continue;
                };
                let source = spec_path
                    .strip_prefix(&root)
                    .unwrap_or(&spec_path)
                    .display()
                    .to_string();
                rows.extend(spec.operations.iter().map(|op| RouteRow {
                    method: op.method.clone(),
                    path: op.path.clone(),
                    source: source.clone(),
                    request: rest::openapi::operation_http(op, &spec.base_url),
                    framework: None,
                }));
            }
            rows
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let rows = detect.await;
            this.update(cx, |this, cx| {
                this.routes = rows;
                this.loaded = true;
                this.task = None;
                this.refilter(cx);
            })
            .ok();
        }));
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.filter.read(cx).text(cx);
        let keys: Vec<String> = self
            .routes
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect();
        self.matches = if query.trim().is_empty() {
            (0..self.routes.len()).collect()
        } else {
            fuzzy::fuzzy_match(keys.iter().map(String::as_str), &query, 500, false)
                .into_iter()
                .map(|m| m.index)
                .collect()
        };
        cx.notify();
    }

    fn method_color(method: &str, theme: &Theme) -> gpui::Hsla {
        match method {
            "GET" => theme.git_added,
            "POST" => theme.accent,
            "PUT" | "PATCH" => theme.warning,
            "DELETE" => theme.error,
            _ => theme.fg_subtle,
        }
    }

    fn render_row(&self, ix: usize, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let route = self.routes.get(*self.matches.get(ix)?)?.clone();
        let (open, send) = (route.request.clone(), route.request.clone());
        Some(
            div()
                .w_full()
                .px_1p5()
                .child(
                    div()
                        .id(("route", ix))
                        .debug_selector(move || format!("route-{ix}"))
                        .relative()
                        .group("route-row")
                        .h(ROW_HEIGHT)
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(8.))
                        .text_size(UI_FONT_SIZE)
                        .overflow_hidden()
                        .hover(|d| d.bg(theme.accent_soft))
                        .child(
                            div()
                                .w(px(52.))
                                .flex_none()
                                .text_size(px(11.))
                                .font_family(crate::theme::CODE_FONT)
                                .text_color(Self::method_color(&route.method, theme))
                                .child(route.method.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .max_w(px(170.))
                                .truncate()
                                .font_family(crate::theme::CODE_FONT)
                                .text_color(theme.fg)
                                .child(route.path.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(11.))
                                .text_color(theme.fg_subtle)
                                .child(route.source.clone()),
                        )
                        .child(
                            div()
                                .absolute()
                                .right(px(4.))
                                .top_0()
                                .h_full()
                                .pl_2()
                                .flex()
                                .items_center()
                                .bg(theme.bg_sunken)
                                .invisible()
                                .group_hover("route-row", |s| s.visible())
                                .child(
                                    div()
                                        .id(("route-send", ix))
                                        .debug_selector(move || format!("route-send-{ix}"))
                                        .h(px(20.))
                                        .px_1p5()
                                        .flex()
                                        .items_center()
                                        .rounded(px(6.))
                                        .text_size(px(11.))
                                        .text_color(theme.fg_subtle)
                                        .hover(|d| d.bg(theme.line).text_color(theme.fg))
                                        .child("Send")
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.stop_propagation();
                                            cx.emit(ApiEvent::Send(send.clone()));
                                        })),
                                ),
                        )
                        .on_click(
                            cx.listener(move |_, _, _, cx| cx.emit(ApiEvent::Open(open.clone()))),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The framework of the first route found in code.
    pub fn framework(&self) -> Option<&'static str> {
        self.routes.iter().find_map(|r| r.framework)
    }

    #[cfg(test)]
    pub fn filter_field(&self) -> Entity<Editor> {
        self.filter.clone()
    }

    #[cfg(test)]
    pub fn visible_rows(&self) -> Vec<RouteRow> {
        self.matches
            .iter()
            .map(|&i| self.routes[i].clone())
            .collect()
    }
}

impl Focusable for ApiPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ApiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.matches.len();
        let focused = self.filter.focus_handle(cx).is_focused(window);
        let empty: Option<SharedString> = if !self.loaded {
            Some("Looking for routes...".into())
        } else if self.routes.is_empty() {
            Some(
                "No routes found. Import an OpenAPI file, or write requests in a .http file."
                    .into(),
            )
        } else {
            None
        };
        div()
            .key_context("ApiPanel")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &RefreshRoutes, _, cx| this.reload(cx)))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .pb_2()
                    .flex()
                    .gap_1p5()
                    .child(
                        ui::button(
                            "api-requests",
                            "Requests",
                            false,
                            &theme,
                            |_, window, cx| window.dispatch_action(Box::new(OpenRequests), cx),
                        )
                        .flex_1(),
                    )
                    .child(ui::button(
                        "api-import",
                        "Import OpenAPI",
                        false,
                        &theme,
                        |_, window, cx| window.dispatch_action(Box::new(ImportOpenApi), cx),
                    ))
                    .child(ui::button(
                        "api-refresh",
                        "Refresh",
                        false,
                        &theme,
                        |_, window, cx| window.dispatch_action(Box::new(RefreshRoutes), cx),
                    )),
            )
            .child(div().flex_none().px_2().pb_2().flex().child(ui::text_field(
                self.filter.clone(),
                focused,
                &theme,
            )))
            .children(empty.map(|text| {
                div()
                    .px_3()
                    .text_size(UI_FONT_SIZE)
                    .text_color(theme.fg_subtle)
                    .child(text)
            }))
            .child(
                uniform_list(
                    "routes",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let theme = cx.theme().clone();
                        range
                            .filter_map(|ix| this.render_row(ix, &theme, cx))
                            .collect()
                    }),
                )
                .flex_1(),
            )
    }
}
