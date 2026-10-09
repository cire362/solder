//! The Git tab in the sidebar: branch, commit box and changed files.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    MouseButton, PromptLevel, SharedString, Subscription, Task, Window, actions, div, prelude::*,
    px, uniform_list,
};

use crate::{
    editor::Editor,
    git::{Change, DiffScope, FileStatus},
    git_store::GitStore,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(
    git,
    [
        Commit,
        StageAll,
        UnstageAll,
        Push,
        ReviewAndPush,
        Pull,
        OpenPullRequest,
        SwitchBranch,
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("secondary-enter", Commit, Some("GitPanel"))]);
}

/// A section header's bulk action (stage all, unstage all).
type SectionAction = fn(&mut GitPanel, &mut Window, &mut Context<GitPanel>);

/// Every row has the same height: `uniform_list` requires it.
const ROW_HEIGHT: gpui::Pixels = px(24.);

pub enum GitPanelEvent {
    OpenFile(PathBuf),
    /// A file at a 1-based line, from the review.
    OpenAt(PathBuf, u32),
    OpenConflict(PathBuf),
    ReviewDiff(PathBuf, DiffScope),
}

impl EventEmitter<GitPanelEvent> for GitPanel {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Conflicts,
    Staged,
    Changes,
    Untracked,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Section::Conflicts => "Conflicts",
            Section::Staged => "Staged",
            Section::Changes => "Changes",
            Section::Untracked => "Untracked",
        }
    }
}

#[derive(Clone)]
enum Row {
    Header(Section, usize),
    File(Section, FileStatus),
}

/// The AI review of a push, shown above the commit box.
#[derive(Clone, Debug, PartialEq)]
pub enum PushReview {
    Running(String),
    Found {
        review: ai::review::Review,
        summary: String,
    },
    Failed(SharedString),
}

pub struct GitPanel {
    root: PathBuf,
    git: Entity<GitStore>,
    pub(crate) message: Entity<Editor>,
    amend: bool,
    pub review: Option<PushReview>,
    review_task: Option<Task<()>>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl GitPanel {
    pub fn new(root: PathBuf, git: Entity<GitStore>, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| Editor::single_line("Commit message", cx));
        let subscription = cx.observe(&git, |_, _, cx| cx.notify());
        Self {
            root,
            git,
            message,
            amend: false,
            review: None,
            review_task: None,
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    /// Push, after the chat model reviewed what it sends when review is on.
    /// Nothing found: the push goes ahead. Something found: it waits.
    fn review_and_push(&mut self, _: &ReviewAndPush, window: &mut Window, cx: &mut Context<Self>) {
        let store = crate::ai_store::AiStore::try_global(cx);
        let model = store.as_ref().and_then(|s| {
            let s = s.read(cx);
            s.review_push
                .then(|| s.roles.get(&ai::Role::Chat).cloned())
                .flatten()
        });
        let (Some(store), Some(model)) = (store, model) else {
            window.dispatch_action(Box::new(Push), cx);
            return;
        };
        if matches!(self.review, Some(PushReview::Running(_))) {
            return;
        }
        let root = self.root.clone();
        self.review = Some(PushReview::Running("Reading what the push sends...".into()));
        cx.notify();
        let outgoing = cx
            .background_executor()
            .spawn(async move { crate::ai_review::outgoing(&root) });
        self.review_task = Some(cx.spawn_in(window, async move |this, cx| {
            let outgoing = match outgoing.await {
                Ok(o) => o,
                Err(e) => {
                    this.update(cx, |this, cx| {
                        this.review = Some(PushReview::Failed(e.into()));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            if outgoing.commits.is_empty() || outgoing.diff.trim().is_empty() {
                // Nothing the model could read: push as asked.
                this.update_in(cx, |this, window, cx| {
                    this.review = None;
                    window.dispatch_action(Box::new(Push), cx);
                })
                .ok();
                return;
            }
            let summary = outgoing.summary();
            this.update(cx, |this, cx| {
                let n = outgoing.commits.len();
                this.review = Some(PushReview::Running(format!(
                    "Reviewing {n} commit{} before the push...",
                    if n == 1 { "" } else { "s" }
                )));
                cx.notify();
            })
            .ok();
            let Ok(endpoint) = store.update(cx, |s, cx| s.endpoint(&model, cx)) else {
                return;
            };
            let name = if model.provider == crate::ai_providers::LOCAL {
                "local".to_string()
            } else {
                model.model.clone()
            };
            let result = async {
                let endpoint = endpoint.await?;
                ai::spawn(ai::review::review(
                    endpoint,
                    name,
                    summary.clone(),
                    outgoing.diff,
                ))
                .await
            }
            .await;
            this.update_in(cx, |this, window, cx| {
                this.review_task = None;
                match result {
                    Ok(review) if review.findings.is_empty() => {
                        this.review = None;
                        window.dispatch_action(Box::new(Push), cx);
                    }
                    Ok(review) => this.review = Some(PushReview::Found { review, summary }),
                    Err(e) => this.review = Some(PushReview::Failed(e.into())),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn end_review(&mut self, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.review = None;
        self.review_task = None;
        if push {
            window.dispatch_action(Box::new(Push), cx);
        }
        cx.notify();
    }

    fn render_review(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let review = self.review.as_ref()?;
        let small = crate::theme::UI_FONT_SMALL;
        let mut block = div()
            .p_2()
            .rounded(theme.shape.control)
            .border(theme.shape.border)
            .flex()
            .flex_col()
            .gap_1();
        let buttons = |push_label: &'static str, cx: &mut Context<Self>| {
            div()
                .pt_1()
                .flex()
                .gap_1p5()
                .child(
                    ui::button(
                        "review-push",
                        push_label,
                        false,
                        theme,
                        cx.listener(|this, _, window, cx| this.end_review(true, window, cx)),
                    )
                    .debug_selector(|| "review-push".into()),
                )
                .child(
                    ui::button(
                        "review-cancel",
                        "Cancel",
                        false,
                        theme,
                        cx.listener(|this, _, window, cx| this.end_review(false, window, cx)),
                    )
                    .debug_selector(|| "review-cancel".into()),
                )
        };
        match review {
            PushReview::Running(text) => {
                block = block
                    .border_color(theme.line)
                    .child(
                        div()
                            .text_size(small)
                            .text_color(theme.fg_muted)
                            .child(text.clone()),
                    )
                    .child(buttons("Push without review", cx));
            }
            PushReview::Failed(e) => {
                block = block
                    .border_color(theme.line)
                    .child(
                        div()
                            .text_size(small)
                            .text_color(theme.error)
                            .child(format!("The review failed: {e}")),
                    )
                    .child(buttons("Push anyway", cx));
            }
            PushReview::Found { review, .. } => {
                let n = review.findings.len();
                block = block.border_color(theme.warning).child(
                    div().text_size(small).text_color(theme.fg).child(format!(
                        "The review found {n} problem{} before the push:",
                        if n == 1 { "" } else { "s" }
                    )),
                );
                for (i, f) in review.findings.iter().enumerate() {
                    let color = match f.severity {
                        ai::review::Severity::Bug => theme.error,
                        ai::review::Severity::Risk => theme.warning,
                        ai::review::Severity::Note => theme.fg_subtle,
                    };
                    let place = if f.line > 0 {
                        format!("{}:{}", f.file, f.line)
                    } else {
                        f.file.clone()
                    };
                    let (path, line) = (self.root.join(&f.file), f.line);
                    block = block.child(
                        div()
                            .id(("review-finding", i))
                            .debug_selector(move || format!("review-finding-{i}"))
                            .px_1()
                            .py_0p5()
                            .rounded(theme.shape.token)
                            .hover(|d| d.bg(theme.bg_elev))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .gap_1p5()
                                    .text_size(crate::theme::text(11.))
                                    .child(div().text_color(color).child(f.severity.label()))
                                    .child(
                                        div()
                                            .truncate()
                                            .font_family(crate::theme::CODE_FONT)
                                            .text_color(theme.fg_subtle)
                                            .child(place),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(small)
                                    .text_color(theme.fg)
                                    .child(f.message.clone()),
                            )
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(GitPanelEvent::OpenAt(path.clone(), line))
                            })),
                    );
                }
                if !review.note.is_empty() {
                    block = block.child(
                        div()
                            .text_size(small)
                            .text_color(theme.fg_subtle)
                            .child(review.note.clone()),
                    );
                }
                block = block.child(buttons("Push anyway", cx));
            }
        }
        Some(block.into_any_element())
    }

    pub fn focus_message(&self, window: &mut Window, cx: &App) {
        window.focus(&self.message.focus_handle(cx));
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        let status = self.git.read(cx).status().clone();
        let mut groups: [(Section, Vec<FileStatus>); 4] = [
            (Section::Conflicts, Vec::new()),
            (Section::Staged, Vec::new()),
            (Section::Changes, Vec::new()),
            (Section::Untracked, Vec::new()),
        ];
        for f in &status.files {
            if f.conflicted {
                groups[0].1.push(f.clone());
                continue;
            }
            if f.untracked {
                groups[3].1.push(f.clone());
                continue;
            }
            if f.staged.is_some() {
                groups[1].1.push(f.clone());
            }
            if f.unstaged.is_some() {
                groups[2].1.push(f.clone());
            }
        }
        let mut rows = Vec::new();
        for (section, files) in groups {
            if files.is_empty() {
                continue;
            }
            rows.push(Row::Header(section, files.len()));
            rows.extend(files.into_iter().map(|f| Row::File(section, f)));
        }
        rows
    }

    fn absolute(&self, rel: &str, cx: &App) -> Option<PathBuf> {
        self.git.read(cx).repo().map(|r| r.workdir.join(rel))
    }

    fn stage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        self.git.update(cx, |g, cx| {
            g.run(
                move |repo| repo.stage(&paths.iter().map(String::as_str).collect::<Vec<_>>()),
                cx,
            )
            .detach()
        });
    }

    fn unstage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        self.git.update(cx, |g, cx| {
            g.run(
                move |repo| repo.unstage(&paths.iter().map(String::as_str).collect::<Vec<_>>()),
                cx,
            )
            .detach()
        });
    }

    fn discard(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Discard changes to {path}?"),
            Some("This restores the staged version. It cannot be undone."),
            &["Discard", "Cancel"],
            cx,
        );
        let git = self.git.clone();
        cx.spawn(async move |_, cx| {
            if answer.await.ok() == Some(0) {
                git.update(cx, |g, cx| {
                    g.run(move |repo| repo.discard(&[&path]), cx).detach()
                })
                .ok();
            }
        })
        .detach();
    }

    fn stage_all(&mut self, _: &StageAll, _: &mut Window, cx: &mut Context<Self>) {
        self.stage(vec![".".into()], cx);
    }

    fn unstage_all(&mut self, _: &UnstageAll, _: &mut Window, cx: &mut Context<Self>) {
        self.unstage(vec![".".into()], cx);
    }

    fn commit(&mut self, _: &Commit, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.message.read(cx).text(cx).trim().to_string();
        let has_staged = self
            .git
            .read(cx)
            .status()
            .files
            .iter()
            .any(|f| f.staged.is_some());
        if message.is_empty() || (!has_staged && !self.amend) {
            self.focus_message(window, cx);
            return;
        }
        let amend = self.amend;
        let task = self.git.update(cx, |g, cx| {
            g.run(move |repo| repo.commit(&message, amend).map(drop), cx)
        });
        let editor = self.message.clone();
        cx.spawn_in(window, async move |this, cx| {
            if task.await {
                editor.update(cx, |e, cx| e.set_text("", false, cx)).ok();
                this.update(cx, |this, cx| {
                    this.amend = false;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn render_row(
        &self,
        ix: usize,
        row: &Row,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match row {
            Row::Header(section, count) => {
                let section = *section;
                let action: Option<(&'static str, SectionAction)> = match section {
                    Section::Staged => Some(("Unstage all", |this, w, cx| {
                        this.unstage_all(&UnstageAll, w, cx)
                    })),
                    Section::Changes | Section::Untracked => {
                        Some(("Stage all", |this, w, cx| this.stage_all(&StageAll, w, cx)))
                    }
                    Section::Conflicts => None,
                };
                div()
                    .id(ix)
                    .h(crate::theme::row(ROW_HEIGHT, cx))
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(crate::theme::text(11.5))
                    .text_color(theme.fg_subtle)
                    .child(format!("{} ({count})", section.title()))
                    .children(action.map(|(label, f)| {
                        div()
                            .id(("section-action", ix))
                            .px_1p5()
                            .rounded(theme.shape.token)
                            .hover(|d| d.bg(theme.line).text_color(theme.fg))
                            .child(label)
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, w, cx| f(this, w, cx)),
                            )
                    }))
                    .into_any_element()
            }
            Row::File(section, file) => {
                let section = *section;
                let (dir, name) = match file.path.rfind('/') {
                    Some(i) => (&file.path[..i], &file.path[i + 1..]),
                    None => ("", file.path.as_str()),
                };
                let change = match section {
                    Section::Staged => file.staged,
                    Section::Changes => file.unstaged,
                    _ => None,
                };
                let (letter, color) = match (section, change) {
                    (Section::Conflicts, _) => ("!", theme.error),
                    (Section::Untracked, _) => ("U", theme.git_added),
                    (_, Some(Change::Added)) => ("A", theme.git_added),
                    (_, Some(Change::Deleted)) => ("D", theme.error),
                    (_, Some(c)) => (c.letter(), theme.git_modified),
                    (_, None) => ("·", theme.fg_subtle),
                };
                let path = file.path.clone();
                let open_path = file.path.clone();
                let deleted = change == Some(Change::Deleted);
                let mut buttons: Vec<gpui::AnyElement> = Vec::new();
                let button = |id: &'static str, label: &'static str| {
                    div()
                        .id((id, ix))
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.shape.token)
                        .text_color(theme.fg_subtle)
                        .hover(|d| d.bg(theme.line).text_color(theme.fg))
                        .child(label)
                };
                match section {
                    Section::Staged => {
                        let p = path.clone();
                        buttons.push(
                            button("unstage", "−")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.unstage(vec![p.clone()], cx)
                                }))
                                .into_any_element(),
                        );
                    }
                    Section::Changes | Section::Untracked | Section::Conflicts => {
                        if section == Section::Changes {
                            let p = path.clone();
                            buttons.push(
                                button("discard", "↺")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.discard(p.clone(), window, cx)
                                    }))
                                    .into_any_element(),
                            );
                        }
                        let p = path.clone();
                        buttons.push(
                            button("stage", "+")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.stage(vec![p.clone()], cx)
                                }))
                                .into_any_element(),
                        );
                    }
                }
                div()
                    .id(ix)
                    .group("git-row")
                    .h(crate::theme::row(ROW_HEIGHT, cx))
                    .mx_1p5()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(theme.shape.control)
                    .text_size(UI_FONT_SIZE)
                    .hover(|d| d.bg(theme.accent_soft))
                    .child(
                        div()
                            .w(px(12.))
                            .flex_none()
                            .text_size(crate::theme::text(11.))
                            .font_family(crate::theme::CODE_FONT)
                            .text_color(color)
                            .child(letter),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(if deleted { theme.fg_subtle } else { theme.fg })
                            .when(deleted, |d| d.line_through())
                            .child(name.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::theme::text(11.))
                            .text_color(theme.fg_subtle)
                            .child(dir.to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .invisible()
                            .group_hover("git-row", |d| d.visible())
                            .children(buttons),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            if let Some(abs) = this.absolute(&open_path, cx) {
                                cx.emit(if section == Section::Conflicts {
                                    GitPanelEvent::OpenConflict(abs)
                                } else if event.click_count == 2 && !deleted {
                                    GitPanelEvent::OpenFile(abs)
                                } else {
                                    GitPanelEvent::ReviewDiff(
                                        abs,
                                        if section == Section::Staged {
                                            DiffScope::Staged
                                        } else {
                                            DiffScope::Working
                                        },
                                    )
                                });
                            }
                        }),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Focusable for GitPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let git = self.git.read(cx);
        if git.is_not_a_repo() {
            return div()
                .size_full()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .text_size(UI_FONT_SIZE)
                .text_color(theme.fg_muted)
                .child("This folder is not a git repository.")
                .child(ui::button(
                    "git-init",
                    "Initialize repository",
                    true,
                    &theme,
                    {
                        let git = self.git.clone();
                        let root = self.root.clone();
                        move |_, _, cx| git.update(cx, |g, cx| g.init(root.clone(), cx))
                    },
                ))
                .into_any_element();
        }
        let status = git.status().clone();
        let error = git.last_error.clone();
        let rows = self.rows(cx);
        let count = rows.len();
        let branch: SharedString = status
            .branch
            .clone()
            .unwrap_or_else(|| "detached HEAD".into())
            .into();
        let sync = match (status.ahead, status.behind) {
            (0, 0) if status.upstream.is_some() => String::new(),
            (0, 0) => "no upstream".into(),
            (a, 0) => format!("↑{a}"),
            (0, b) => format!("↓{b}"),
            (a, b) => format!("↑{a} ↓{b}"),
        };
        let message_focused = self.message.focus_handle(cx).is_focused(window);
        let amend = self.amend;
        div()
            .key_context("GitPanel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::commit))
            .on_action(cx.listener(Self::review_and_push))
            .on_action(cx.listener(Self::stage_all))
            .on_action(cx.listener(Self::unstage_all))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .id("branch")
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(26.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .rounded(theme.shape.control)
                                    .text_size(UI_FONT_SIZE)
                                    .text_color(theme.fg)
                                    .hover(|d| d.bg(theme.line))
                                    .child(div().truncate().child(branch))
                                    .child(div().text_color(theme.fg_subtle).child(sync))
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(SwitchBranch), cx)
                                    }),
                            )
                            .child(
                                ui::button("git-push", "Push", false, &theme, |_, window, cx| {
                                    window.dispatch_action(Box::new(ReviewAndPush), cx)
                                })
                                .debug_selector(|| "git-push".into()),
                            )
                            .child(ui::button(
                                "git-pr",
                                "PR",
                                false,
                                &theme,
                                |_, window, cx| {
                                    window.dispatch_action(Box::new(OpenPullRequest), cx)
                                },
                            )),
                    )
                    .children(self.render_review(&theme, cx))
                    .child(
                        ui::text_field(self.message.clone(), message_focused, &theme)
                            .flex_none()
                            .w_full(),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                ui::button(
                                    "git-commit",
                                    if amend { "Amend" } else { "Commit" },
                                    true,
                                    &theme,
                                    {
                                        let this = cx.entity();
                                        move |_, window, cx| {
                                            this.update(cx, |p, cx| p.commit(&Commit, window, cx))
                                        }
                                    },
                                )
                                .flex_1(),
                            )
                            .child(ui::toggle(
                                "git-amend",
                                "amend",
                                "Amend last commit",
                                amend,
                                &theme,
                                {
                                    let this = cx.entity();
                                    move |_, _, cx| {
                                        this.update(cx, |p, cx| {
                                            p.amend = !p.amend;
                                            cx.notify();
                                        })
                                    }
                                },
                            )),
                    )
                    .children(error.map(|e| {
                        div()
                            .px_2()
                            .py_1()
                            .rounded(theme.shape.control)
                            .border(theme.shape.border)
                            .border_color(theme.error)
                            .text_size(crate::theme::text(11.5))
                            .text_color(theme.error)
                            .child(e)
                    })),
            )
            .child(if count == 0 {
                div()
                    .px_4()
                    .py_2()
                    .text_size(UI_FONT_SIZE)
                    .text_color(theme.fg_subtle)
                    .child("No changes")
                    .into_any_element()
            } else {
                uniform_list(
                    "git-files",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let theme = cx.theme().clone();
                        let rows = this.rows(cx);
                        range
                            .filter_map(|ix| {
                                rows.get(ix).map(|r| this.render_row(ix, r, &theme, cx))
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .into_any_element()
            })
            .into_any_element()
    }
}

/// Switch branch, or create one from the query.
pub struct BranchPicker {
    git: Entity<GitStore>,
    branches: Vec<String>,
    query: String,
    /// `None` is the "create branch" row.
    matches: Vec<(Option<usize>, Vec<u32>)>,
    selected: usize,
}

impl BranchPicker {
    pub fn new(git: Entity<GitStore>) -> Self {
        Self {
            git,
            branches: Vec::new(),
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
        }
    }

    /// Loads branch names in the background, then refilters.
    pub fn load(
        picker: &mut crate::picker::Picker<Self>,
        window: &mut Window,
        cx: &mut Context<crate::picker::Picker<Self>>,
    ) {
        let Some(repo) = picker.delegate.git.read(cx).repo().cloned() else {
            return;
        };
        cx.spawn_in(window, async move |picker, cx| {
            let branches = cx
                .background_executor()
                .spawn(async move { repo.branches().unwrap_or_default() })
                .await;
            picker
                .update_in(cx, |picker, window, cx| {
                    picker.delegate.branches = branches;
                    picker.refresh(window, cx);
                })
                .ok();
        })
        .detach();
    }
}

impl crate::picker::PickerDelegate for BranchPicker {
    fn placeholder(&self) -> SharedString {
        "Switch to branch, or type a new name".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Context<crate::picker::Picker<Self>>) {
        self.selected = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        _: &mut Window,
        _: &mut Context<crate::picker::Picker<Self>>,
    ) -> gpui::Task<()> {
        let query = query.trim().to_string();
        self.matches = if query.is_empty() {
            (0..self.branches.len())
                .map(|i| (Some(i), Vec::new()))
                .collect()
        } else {
            crate::fuzzy::fuzzy_match(self.branches.iter().map(String::as_str), &query, 100, true)
                .into_iter()
                .map(|m| {
                    let positions = crate::fuzzy::positions(&self.branches[m.index], &query, true);
                    (Some(m.index), positions)
                })
                .collect()
        };
        if !query.is_empty() && !self.branches.contains(&query) {
            self.matches.push((None, Vec::new()));
        }
        self.query = query;
        self.selected = 0;
        gpui::Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<crate::picker::Picker<Self>>) {
        let Some((ix, _)) = self.matches.get(self.selected) else {
            return;
        };
        let (name, create) = match ix {
            Some(i) => (self.branches[*i].clone(), false),
            None => (self.query.clone(), true),
        };
        self.git.update(cx, |g, cx| {
            g.run(move |repo| repo.switch(&name, create), cx).detach()
        });
        cx.emit(gpui::DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<crate::picker::Picker<Self>>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let (branch, positions) = &self.matches[ix];
        let content = match branch {
            Some(i) => crate::picker::highlighted_text(
                &self.branches[*i],
                positions,
                theme.fg,
                theme.accent,
            )
            .into_any_element(),
            None => format!("Create branch \u{201c}{}\u{201d}", self.query).into_any_element(),
        };
        div()
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .child(content)
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        "Loading branches...".into()
    }
}
