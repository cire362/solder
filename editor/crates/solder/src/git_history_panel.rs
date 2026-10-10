use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, SharedString, Task, Window, div,
    prelude::*, px, uniform_list,
};

use crate::{
    git::Repo,
    git_history::Row,
    theme::{ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
    ui,
};

pub enum HistoryEvent {
    Close,
    Patch { title: String, text: String },
}

pub struct HistoryPanel {
    repo: Repo,
    file: Option<String>,
    pub(crate) rows: Vec<Row>,
    limit: usize,
    loading: bool,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    patch_task: Option<Task<()>>,
    focus: FocusHandle,
}
impl EventEmitter<HistoryEvent> for HistoryPanel {}
impl Focusable for HistoryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl HistoryPanel {
    pub fn new(repo: Repo, file: Option<String>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            repo,
            file,
            rows: Vec::new(),
            limit: 200,
            loading: false,
            error: None,
            task: None,
            patch_task: None,
            focus: cx.focus_handle(),
        };
        panel.load(cx);
        panel
    }
    fn load(&mut self, cx: &mut Context<Self>) {
        let (repo, file, limit) = (self.repo.clone(), self.file.clone(), self.limit);
        self.loading = true;
        self.error = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repo.history(file.as_deref(), limit) })
                .await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(rows) => this.rows = rows,
                    Err(e) => this.error = Some(e.0.into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
    fn open(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(commit) = self.rows.get(ix).and_then(|r| r.commit.clone()) else {
            return;
        };
        let (repo, file) = (self.repo.clone(), self.file.clone());
        self.patch_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repo.commit_patch(&commit.id, file.as_deref()) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(text) => cx.emit(HistoryEvent::Patch {
                        title: commit.subject,
                        text,
                    }),
                    Err(e) => this.error = Some(e.0.into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }
}
impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(UI_FONT_SIZE)
            .track_focus(&self.focus)
            .child(
                div()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                ui::button(
                                    "history-back",
                                    "Changes",
                                    false,
                                    &theme,
                                    cx.listener(|_, _, _, cx| cx.emit(HistoryEvent::Close)),
                                )
                                .debug_selector(|| "history-back".into()),
                            )
                            .child(ui::button(
                                "history-refresh",
                                "Refresh",
                                false,
                                &theme,
                                cx.listener(|this, _, _, cx| this.load(cx)),
                            )),
                    )
                    .child(
                        div()
                            .text_color(theme.fg)
                            .truncate()
                            .child(self.file.clone().unwrap_or_else(|| "All branches".into())),
                    )
                    .children(self.loading.then(|| {
                        div()
                            .text_color(theme.fg_subtle)
                            .child("Reading history...")
                    }))
                    .children(
                        self.error
                            .clone()
                            .map(|e| div().text_color(theme.error).child(e)),
                    )
                    .child(ui::button(
                        "history-more",
                        "Older commits",
                        false,
                        &theme,
                        cx.listener(|this, _, _, cx| {
                            this.limit += 200;
                            this.load(cx);
                        }),
                    )),
            )
            .when(
                !self.loading && self.rows.is_empty() && self.error.is_none(),
                |d| d.child(div().p_2().text_color(theme.fg_subtle).child("No commits")),
            )
            .child(
                uniform_list(
                    "git-history",
                    self.rows.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        let theme = cx.theme().clone();
                        range
                            .filter_map(|ix| {
                                this.rows.get(ix).map(|row| {
                                    let mut d = div()
                                        .id(ix)
                                        .debug_selector(move || format!("history-row-{ix}"))
                                        .h(crate::theme::row(px(44.), cx))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .hover(|d| d.bg(theme.bg_elev))
                                        .child(
                                            div()
                                                .font_family(crate::theme::CODE_FONT)
                                                .text_color(theme.accent)
                                                .child(row.graph.clone()),
                                        );
                                    if let Some(c) = &row.commit {
                                        d = d
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex()
                                                    .flex_col()
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_color(theme.fg)
                                                            .child(format!(
                                                                "{} {}",
                                                                c.subject, c.refs
                                                            )),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_size(UI_FONT_SMALL)
                                                            .text_color(theme.fg_subtle)
                                                            .child(format!(
                                                                "{} {} {}",
                                                                &c.id[..7],
                                                                c.author,
                                                                &c.date[..10.min(c.date.len())]
                                                            )),
                                                    ),
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.open(ix, cx)
                                                }),
                                            );
                                    }
                                    d
                                })
                            })
                            .collect()
                    }),
                )
                .flex_1(),
            )
    }
}
