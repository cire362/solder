use crate::{
    git::Repo,
    git_pr::{PullRequest, State},
    theme::{ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
    ui,
};
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, SharedString, Task, Window, div,
    prelude::*, px, uniform_list,
};
use std::path::PathBuf;

pub struct Close;
pub struct PullRequestPanel {
    repo: Repo,
    pub branch: String,
    program: PathBuf,
    data: Option<PullRequest>,
    loading: bool,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    focus: FocusHandle,
}
impl EventEmitter<Close> for PullRequestPanel {}
impl Focusable for PullRequestPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl PullRequestPanel {
    pub fn new(repo: Repo, branch: String, cx: &mut Context<Self>) -> Self {
        Self::with_program(repo, branch, "gh".into(), cx)
    }
    fn with_program(repo: Repo, branch: String, program: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            repo,
            branch,
            program,
            data: None,
            loading: false,
            error: None,
            task: None,
            focus: cx.focus_handle(),
        };
        panel.load(cx);
        panel
    }
    fn load(&mut self, cx: &mut Context<Self>) {
        let (repo, branch, program) =
            (self.repo.clone(), self.branch.clone(), self.program.clone());
        self.loading = true;
        self.error = None;
        self.data = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::git_pr::read(&repo, &branch, &program) })
                .await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(pr) => this.data = Some(pr),
                    Err(e) => this.error = Some(e.0.into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}
impl Render for PullRequestPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut panel = div()
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
                            .child(ui::button(
                                "pr-back",
                                "Changes",
                                false,
                                &theme,
                                cx.listener(|_, _, _, cx| cx.emit(Close)),
                            ))
                            .child(
                                ui::button(
                                    "pr-refresh",
                                    "Refresh",
                                    false,
                                    &theme,
                                    cx.listener(|this, _, _, cx| this.load(cx)),
                                )
                                .debug_selector(|| "pr-refresh".into()),
                            ),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_color(theme.fg)
                            .child(self.branch.clone()),
                    )
                    .child(ui::button(
                        "pr-create",
                        "Create PR",
                        false,
                        &theme,
                        |_, window, cx| {
                            window
                                .dispatch_action(Box::new(crate::git_panel::CreatePullRequest), cx)
                        },
                    ))
                    .children(self.loading.then(|| {
                        div()
                            .text_color(theme.fg_subtle)
                            .child("Reading pull request...")
                    }))
                    .children(
                        self.error
                            .clone()
                            .map(|e| div().text_color(theme.error).child(e)),
                    ),
            );
        if let Some(pr) = &self.data {
            let url = pr.url.clone();
            panel = panel
                .child(
                    div()
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_color(theme.fg)
                                .child(format!("#{} {}", pr.number, pr.title)),
                        )
                        .child(
                            div()
                                .text_size(UI_FONT_SMALL)
                                .text_color(theme.fg_muted)
                                .child(format!(
                                    "{}{} · {} -> {}",
                                    pr.state,
                                    if pr.draft { " · Draft" } else { "" },
                                    pr.head,
                                    pr.base
                                )),
                        )
                        .child(ui::button(
                            "pr-open",
                            "Open in browser",
                            false,
                            &theme,
                            move |_, _, cx| cx.open_url(&url),
                        ))
                        .children(pr.checks.is_empty().then(|| {
                            div()
                                .text_color(theme.fg_subtle)
                                .child("No checks reported")
                        })),
                )
                .child(
                    uniform_list(
                        "pr-checks",
                        pr.checks.len(),
                        cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                            let theme = cx.theme().clone();
                            range
                                .filter_map(|ix| {
                                    this.data.as_ref()?.checks.get(ix).map(|check| {
                                        let color = match check.state {
                                            State::Passed => theme.git_added,
                                            State::Failed => theme.error,
                                            State::Pending => theme.warning,
                                            _ => theme.fg_subtle,
                                        };
                                        let url = check.url.clone();
                                        div()
                                            .id(ix)
                                            .h(crate::theme::row(px(30.), cx))
                                            .px_2()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .hover(|d| d.bg(theme.bg_elev))
                                            .text_size(UI_FONT_SMALL)
                                            .child(
                                                div().text_color(color).child(check.state.label()),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_color(theme.fg)
                                                    .child(check.name.clone()),
                                            )
                                            .on_click(move |_, _, cx| {
                                                if let Some(url) = &url {
                                                    cx.open_url(url);
                                                }
                                            })
                                    })
                                })
                                .collect()
                        }),
                    )
                    .flex_1(),
                );
        }
        panel
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    #[gpui::test]
    fn the_checks_refresh_only_when_asked(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let f = crate::git_history::tests::Fixture::new("pr-panel");
        let file = f.0.workdir.join("response.json");
        let program = f.0.workdir.join("fake-gh");
        std::fs::write(&program, "#!/bin/sh\ncat response.json\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut data = serde_json::json!({"number":5,"title":"Example","url":"https://github.com/a/b/pull/5","state":"OPEN","headRefName":"feature","baseRefName":"main","statusCheckRollup":[{"name":"Build","status":"COMPLETED","conclusion":"FAILURE"}]});
        std::fs::write(&file, data.to_string()).unwrap();
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::dark());
            cx.set_global(crate::settings::Settings::default());
        });
        let (panel, cx) = cx.add_window_view(|_, cx| {
            PullRequestPanel::with_program(f.0.clone(), "feature".into(), program, cx)
        });
        let wait = |cx: &mut gpui::VisualTestContext, state| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                cx.run_until_parked();
                if cx.read(|cx| {
                    panel
                        .read(cx)
                        .data
                        .as_ref()
                        .is_some_and(|d| d.checks[0].state == state)
                }) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("PR state was not read");
        };
        wait(cx, State::Failed);
        data["statusCheckRollup"][0]["conclusion"] = "SUCCESS".into();
        std::fs::write(&file, data.to_string()).unwrap();
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| panel.read(cx).data.as_ref().unwrap().checks[0].state),
            State::Failed
        );
        let bounds = cx.debug_bounds("pr-refresh").unwrap();
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        wait(cx, State::Passed);
    }
}
