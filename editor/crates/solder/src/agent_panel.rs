//! The agent on the right: describe a task, approve or edit the plan, allow
//! commands that need more than the sandbox, then review the changes and
//! merge them or throw them away.

use ai::Role;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, KeyBinding, ListAlignment, ListState,
    SharedString, Subscription, WeakEntity, Window, actions, div, list, prelude::*, px,
};

use crate::{
    agent::Access,
    agent_task::{AgentEvent, AgentTask, Entry, Status},
    ai_store::AiStore,
    editor::Editor,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
    workspace::Workspace,
};

actions!(agent, [Submit]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("enter", Submit, Some("AgentInput"))]);
}

pub struct AgentPanel {
    store: Entity<AiStore>,
    workspace: WeakEntity<Workspace>,
    /// The project, fixed for a window; kept here so starting a task never
    /// reads the workspace (which may be mid-update).
    root: std::path::PathBuf,
    pub task: Option<Entity<AgentTask>>,
    input: Entity<Editor>,
    /// The plan while it waits for approval, one field per step.
    plan_fields: Vec<Entity<Editor>>,
    list: ListState,
    error: Option<SharedString>,
    /// Worktrees of tasks from earlier runs.
    leftovers: Vec<crate::agent::Worktree>,
    focus: FocusHandle,
    _task_subscription: Option<Subscription>,
}

impl AgentPanel {
    pub fn new(
        store: Entity<AiStore>,
        workspace: WeakEntity<Workspace>,
        root: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self {
            store,
            workspace,
            root,
            task: None,
            input: cx.new(|cx| Editor::single_line("Describe a task for the agent", cx)),
            plan_fields: Vec::new(),
            list: ListState::new(0, ListAlignment::Bottom, px(400.)),
            error: None,
            leftovers: Vec::new(),
            focus: cx.focus_handle(),
            _task_subscription: None,
        }
    }

    pub fn input(&self) -> Entity<Editor> {
        self.input.clone()
    }

    pub fn shown(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| {
            s.load(cx);
            if s.hardware.is_some() {
                s.refresh_providers(cx);
            }
        });
        self.find_leftovers(cx);
    }

    fn find_leftovers(&mut self, cx: &mut Context<Self>) {
        let repo = self.root.clone();
        let trees = self.store.read(cx).dirs.root.join("worktrees");
        let current = self
            .task
            .as_ref()
            .and_then(|t| t.read(cx).worktree.as_ref().map(|w| w.path.clone()));
        let found = cx
            .background_executor()
            .spawn(async move { crate::agent::leftovers(&repo, &trees) });
        cx.spawn(async move |this, cx| {
            let found = found.await;
            this.update(cx, |this, cx| {
                this.leftovers = found
                    .into_iter()
                    .filter(|w| Some(&w.path) != current.as_ref())
                    .collect();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn remove_leftovers(&mut self, cx: &mut Context<Self>) {
        let trees = std::mem::take(&mut self.leftovers);
        cx.background_executor()
            .spawn(async move {
                for w in trees {
                    w.remove();
                }
            })
            .detach();
        cx.notify();
    }

    fn task_status(&self, cx: &App) -> Option<Status> {
        self.task.as_ref().map(|t| t.read(cx).status.clone())
    }

    /// Enter in the field: a new task, or the next instruction for this one.
    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text(cx);
        if text.trim().is_empty() {
            return;
        }
        let finished = matches!(
            self.task_status(cx),
            None | Some(Status::Merged | Status::Discarded | Status::Failed(_))
        );
        if finished {
            self.start(text, window, cx);
        } else if let Some(task) = &self.task {
            if task.read(cx).status.busy() {
                return;
            }
            task.update(cx, |t, cx| t.follow_up(text, cx));
            self.input.update(cx, |e, cx| e.set_text("", false, cx));
        }
    }

    fn start(&mut self, text: String, _: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.store.read(cx).roles.get(&Role::Chat).cloned() else {
            self.error = Some("Pick a chat model first: the agent uses it.".into());
            cx.notify();
            return;
        };
        let repo = self.root.clone();
        let trees = self.store.read(cx).dirs.root.join("worktrees");
        let store = self.store.clone();
        let task =
            cx.new(|cx| AgentTask::start(text.trim().to_string(), model, repo, trees, store, cx));
        self._task_subscription = Some(cx.subscribe(&task, |this, _, _: &AgentEvent, cx| {
            this.sync(cx);
        }));
        self.task = Some(task);
        self.error = None;
        self.plan_fields.clear();
        self.input.update(cx, |e, cx| e.set_text("", false, cx));
        self.sync(cx);
    }

    /// Follows the task: the log's length, and plan fields while the plan
    /// waits.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(task) = &self.task else {
            return;
        };
        let t = task.read(cx);
        let count = t.entries.len();
        if self.list.item_count() != count {
            self.list.reset(count);
            self.list.scroll_to_reveal_item(count.saturating_sub(1));
        } else if count > 0 {
            // Opening an entry changes its height.
            self.list.reset(count);
        }
        let placeholder = if matches!(
            t.status,
            Status::Merged | Status::Discarded | Status::Failed(_)
        ) {
            "Describe a task for the agent"
        } else {
            "Tell the agent what to do next"
        };
        self.input
            .update(cx, |e, _| e.placeholder = Some(placeholder.into()));
        let t = task.read(cx);
        if t.status == Status::AwaitingPlan {
            if self.plan_fields.len() != t.plan.len() || self.plan_fields.is_empty() {
                let steps: Vec<String> = t.plan.iter().map(|s| s.text.clone()).collect();
                self.plan_fields = steps
                    .into_iter()
                    .map(|text| {
                        cx.new(|cx| {
                            let mut e = Editor::single_line("Step", cx);
                            e.set_text(&text, false, cx);
                            // Show the step from its start.
                            e.select_ranges(std::slice::from_ref(&(0..0)), cx);
                            e
                        })
                    })
                    .collect();
            }
        } else {
            self.plan_fields.clear();
        }
        cx.notify();
    }

    pub fn approve_plan(&mut self, cx: &mut Context<Self>) {
        let steps: Vec<String> = self
            .plan_fields
            .iter()
            .map(|f| f.read(cx).text(cx))
            .collect();
        if let Some(task) = &self.task {
            task.update(cx, |t, cx| t.approve_plan(steps, cx));
        }
    }

    fn add_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| Editor::single_line("Step", cx));
        window.focus(&field.focus_handle(cx));
        self.plan_fields.push(field);
        cx.notify();
    }

    fn review(&mut self, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(task), Some(workspace)) = (self.task.clone(), self.workspace.upgrade()) else {
            return;
        };
        let Some(path) = task.read(cx).worktree.as_ref().map(|w| w.path.join(&rel)) else {
            return;
        };
        let texts = task.update(cx, |t, cx| t.diff_texts(&rel, cx));
        let window_handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            let (old, new) = texts.await;
            cx.update_window(window_handle, |_, window, cx| {
                workspace.update(cx, |w, cx| {
                    w.show_agent_diff(path, rel, old, new, window, cx)
                });
            })
            .ok();
        })
        .detach();
    }

    fn render_entry(&mut self, ix: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(task) = self.task.clone() else {
            return div().into_any_element();
        };
        let Some(entry) = task.read(cx).entries.get(ix).cloned() else {
            return div().into_any_element();
        };
        let small = crate::theme::UI_FONT_SMALL;
        let body = match entry {
            Entry::Text(text) => div()
                .text_size(UI_FONT_SIZE)
                .text_color(theme.fg)
                .child(text),
            Entry::Note(text) => div()
                .px_2()
                .py_1()
                .rounded(theme.shape.control)
                .bg(theme.bg_elev)
                .text_size(small)
                .text_color(theme.fg_muted)
                .child(text),
            Entry::Tool {
                title,
                output,
                error,
                open,
            } => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .id(("agent-entry", ix))
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(small)
                        .text_color(if error {
                            theme.warning
                        } else {
                            theme.fg_subtle
                        })
                        .hover(|d| d.text_color(theme.fg))
                        .child(if open { "▾" } else { "▸" })
                        .child(
                            div()
                                .truncate()
                                .font_family(crate::theme::CODE_FONT)
                                .child(title),
                        )
                        .on_click(move |_, _, cx| task.update(cx, |t, cx| t.toggle_entry(ix, cx))),
                )
                .when(open, |d| {
                    d.child(
                        div()
                            .p_2()
                            .rounded(theme.shape.control)
                            .bg(theme.bg_sunken)
                            .font_family(crate::theme::CODE_FONT)
                            .text_size(crate::theme::text(11.))
                            .text_color(theme.fg_muted)
                            .children(output.lines().take(200).map(|l| div().child(l.to_string()))),
                    )
                }),
        };
        div().px_3().py_1().child(body).into_any_element()
    }

    fn render_plan(
        &self,
        window: &Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let task = self.task.as_ref()?.read(cx);
        if task.plan.is_empty() {
            return None;
        }
        let small = crate::theme::UI_FONT_SMALL;
        let mut block = div()
            .mx_2()
            .mb_2()
            .p_2()
            .rounded(theme.shape.control)
            .border(theme.shape.border)
            .border_color(theme.line)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(crate::theme::text(11.))
                    .text_color(theme.fg_subtle)
                    .child("PLAN"),
            );
        if task.status == Status::AwaitingPlan && !self.plan_fields.is_empty() {
            for (i, field) in self.plan_fields.iter().enumerate() {
                let focused = field.focus_handle(cx).is_focused(window);
                block = block.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .w(px(16.))
                                .text_size(small)
                                .text_color(theme.fg_subtle)
                                .child(format!("{}.", i + 1)),
                        )
                        .child(ui::text_field(field.clone(), focused, theme))
                        .child(ui::toggle(
                            SharedString::from(format!("agent-remove-step-{i}")),
                            "×",
                            "",
                            false,
                            theme,
                            cx.listener(move |this, _, _, cx| {
                                if i < this.plan_fields.len() {
                                    this.plan_fields.remove(i);
                                    cx.notify();
                                }
                            }),
                        )),
                );
            }
            block = block.child(
                div()
                    .pt_1()
                    .flex()
                    .gap_1p5()
                    .child(
                        ui::button(
                            "agent-approve",
                            "Approve plan",
                            true,
                            theme,
                            cx.listener(|this, _, _, cx| this.approve_plan(cx)),
                        )
                        .debug_selector(|| "agent-approve".into()),
                    )
                    .child(ui::button(
                        "agent-add-step",
                        "Add step",
                        false,
                        theme,
                        cx.listener(|this, _, window, cx| this.add_step(window, cx)),
                    )),
            );
            block = block.child(
                div()
                    .text_size(small)
                    .text_color(theme.fg_subtle)
                    .child("Edit the steps, or tell the agent below what to change."),
            );
        } else {
            for step in &task.plan {
                block = block.child(
                    div()
                        .flex()
                        .gap_1p5()
                        .text_size(small)
                        .text_color(if step.done { theme.fg_subtle } else { theme.fg })
                        .child(div().flex_none().child(if step.done { "✓" } else { "○" }))
                        .child(div().flex_1().min_w_0().child(step.text.clone())),
                );
            }
        }
        Some(block.into_any_element())
    }

    fn render_action(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let task_entity = self.task.clone()?;
        let task = task_entity.read(cx);
        let small = crate::theme::UI_FONT_SMALL;
        match &task.status {
            Status::AwaitingApproval {
                command,
                access,
                reason,
            } => {
                let what = match access {
                    Access::Network => "The agent wants to run this with network access:",
                    Access::Full => "The agent wants to run this outside the sandbox:",
                    Access::Sandboxed => "There is no sandbox here. The agent wants to run:",
                };
                let (allow, deny) = (task_entity.clone(), task_entity.clone());
                Some(
                    div()
                        .mx_2()
                        .mb_2()
                        .p_2()
                        .rounded(theme.shape.control)
                        .border(theme.shape.border)
                        .border_color(theme.warning)
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(div().text_size(small).text_color(theme.fg).child(what))
                        .child(
                            div()
                                .p_1p5()
                                .rounded(theme.shape.token)
                                .bg(theme.bg_sunken)
                                .font_family(crate::theme::CODE_FONT)
                                .text_size(crate::theme::text(11.5))
                                .text_color(theme.fg)
                                .child(command.clone()),
                        )
                        .when(!reason.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_size(small)
                                    .text_color(theme.fg_subtle)
                                    .child(reason.clone()),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .gap_1p5()
                                .child(
                                    ui::button(
                                        "agent-allow",
                                        "Allow once",
                                        true,
                                        theme,
                                        move |_, _, cx| {
                                            allow.update(cx, |t, cx| t.decide(true, cx))
                                        },
                                    )
                                    .debug_selector(|| "agent-allow".into()),
                                )
                                .child(
                                    ui::button(
                                        "agent-deny",
                                        "Don't run",
                                        false,
                                        theme,
                                        move |_, _, cx| {
                                            deny.update(cx, |t, cx| t.decide(false, cx))
                                        },
                                    )
                                    .debug_selector(|| "agent-deny".into()),
                                ),
                        )
                        .into_any_element(),
                )
            }
            Status::Finished | Status::Stopped => {
                let mut block = div()
                    .mx_2()
                    .mb_2()
                    .p_2()
                    .rounded(theme.shape.control)
                    .border(theme.shape.border)
                    .border_color(theme.line)
                    .flex()
                    .flex_col()
                    .gap_1();
                if let Some(summary) = &task.summary {
                    block = block.child(
                        div()
                            .text_size(UI_FONT_SIZE)
                            .text_color(theme.fg)
                            .child(summary.clone()),
                    );
                }
                block = block.child(
                    div()
                        .pt_1()
                        .text_size(crate::theme::text(11.))
                        .text_color(theme.fg_subtle)
                        .child(match task.changes.len() {
                            0 => "NO CHANGES".to_string(),
                            1 => "1 FILE CHANGED".to_string(),
                            n => format!("{n} FILES CHANGED"),
                        }),
                );
                for (i, change) in task.changes.iter().enumerate() {
                    let rel = change.path.clone();
                    let color = match change.kind {
                        'A' => theme.git_added,
                        'D' => theme.error,
                        _ => theme.git_modified,
                    };
                    block = block.child(
                        div()
                            .id(("agent-change", i))
                            .debug_selector(move || format!("agent-change-{i}"))
                            .px_1()
                            .rounded(theme.shape.token)
                            .flex()
                            .gap_1p5()
                            .text_size(small)
                            .hover(|d| d.bg(theme.bg_elev))
                            .child(
                                div()
                                    .w(px(12.))
                                    .text_color(color)
                                    .child(change.kind.to_string()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_family(crate::theme::CODE_FONT)
                                    .text_color(theme.fg)
                                    .child(change.path.clone()),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.review(rel.clone(), window, cx)
                            })),
                    );
                }
                let (merge, discard) = (task_entity.clone(), task_entity.clone());
                block = block.child(
                    div()
                        .pt_1()
                        .flex()
                        .gap_1p5()
                        .when(!task.changes.is_empty(), |d| {
                            d.child(
                                ui::button(
                                    "agent-merge",
                                    "Merge into my branch",
                                    true,
                                    theme,
                                    move |_, _, cx| merge.update(cx, |t, cx| t.merge(cx)),
                                )
                                .debug_selector(|| "agent-merge".into()),
                            )
                        })
                        .child(
                            ui::button(
                                "agent-discard",
                                "Discard",
                                false,
                                theme,
                                move |_, _, cx| discard.update(cx, |t, cx| t.discard(cx)),
                            )
                            .debug_selector(|| "agent-discard".into()),
                        ),
                );
                Some(block.into_any_element())
            }
            _ => None,
        }
    }
}

impl Focusable for AgentPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let small = crate::theme::UI_FONT_SMALL;
        let status = self.task_status(cx);
        let model = {
            let s = self.store.read(cx);
            s.roles.get(&Role::Chat).map(|m| s.model_label(m))
        };
        let title = self.task.as_ref().map(|t| t.read(cx).title.clone());
        let status_text: Option<(String, gpui::Hsla)> = status.as_ref().map(|s| match s {
            Status::Starting => ("Making a worktree...".into(), theme.fg_subtle),
            Status::Thinking => {
                let steps = self.task.as_ref().map_or(0, |t| t.read(cx).steps);
                (format!("Working, step {steps}..."), theme.accent)
            }
            Status::AwaitingPlan => ("Waiting for you to approve the plan".into(), theme.accent),
            Status::AwaitingApproval { .. } => {
                ("Waiting for you to allow a command".into(), theme.warning)
            }
            Status::Finished => ("Done; review and merge".into(), theme.git_added),
            Status::Stopped => ("Stopped".into(), theme.fg_subtle),
            Status::Failed(e) => (e.to_string(), theme.error),
            Status::Merged => ("Merged".into(), theme.git_added),
            Status::Discarded => ("Discarded".into(), theme.fg_subtle),
        });
        let busy = status.as_ref().is_some_and(Status::busy);
        let plan = self.render_plan(window, &theme, cx);
        let action = self.render_action(&theme, cx);
        let focused = self.input.focus_handle(cx).is_focused(window);
        let can_type = !busy;
        let placeholder_new = matches!(
            status,
            None | Some(Status::Merged | Status::Discarded | Status::Failed(_))
        );
        let this = cx.entity().downgrade();
        let entries = list(self.list.clone(), move |ix, _, cx| {
            let theme = cx.theme().clone();
            this.upgrade()
                .map(|e| e.update(cx, |p, cx| p.render_entry(ix, &theme, cx)))
                .unwrap_or_else(|| div().into_any_element())
        })
        .flex_1()
        .min_h_0();
        div()
            .key_context("AgentPanel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .border_b(theme.shape.border)
                    .border_color(theme.line)
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(UI_FONT_SIZE)
                                    .text_color(theme.fg)
                                    .child(title.unwrap_or_else(|| "Agent".into())),
                            )
                            .when(busy || matches!(status, Some(Status::AwaitingApproval { .. })), |d| {
                                d.child(
                                    ui::button("agent-stop", "Stop", false, &theme, cx.listener(|this, _, _, cx| {
                                        if let Some(t) = &this.task {
                                            t.update(cx, |t, cx| t.stop(cx));
                                        }
                                    }))
                                    .debug_selector(|| "agent-stop".into()),
                                )
                            }),
                    )
                    .child(match status_text {
                        Some((text, color)) => div().text_size(small).text_color(color).child(text),
                        None => div().text_size(small).text_color(theme.fg_subtle).child(match &model {
                            Some(m) => format!(
                                "Uses {m}. It works on its own branch from your last commit: you approve its plan, allow commands that need more than the sandbox, then review and merge."
                            ),
                            None => "Pick a chat model first; the agent uses it.".into(),
                        }),
                    }),
            )
            .when(!self.leftovers.is_empty(), |d| {
                let n = self.leftovers.len();
                d.child(
                    div()
                        .mx_2()
                        .mt_2()
                        .p_2()
                        .rounded(theme.shape.control)
                        .bg(theme.bg_elev)
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_size(small)
                                .text_color(theme.fg_muted)
                                .child(if n == 1 {
                                    "An earlier task left its branch and worktree.".to_string()
                                } else {
                                    format!("{n} earlier tasks left their branches and worktrees.")
                                }),
                        )
                        .child(
                            ui::button("agent-leftovers", "Remove", false, &theme, cx.listener(|this, _, _, cx| this.remove_leftovers(cx)))
                                .debug_selector(|| "agent-leftovers".into()),
                        ),
                )
            })
            .children(plan)
            .child(entries)
            .children(action)
            .children(self.error.clone().map(|e| {
                div().px_3().pb_1().text_size(small).text_color(theme.error).child(e)
            }))
            .child(
                div()
                    .flex_none()
                    .p_2()
                    .border_t(theme.shape.border)
                    .border_color(theme.line)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .key_context("AgentInput")
                    .on_action(cx.listener(Self::submit))
                    .child(ui::text_field(self.input.clone(), focused, &theme))
                    .child(
                        ui::button(
                            "agent-submit",
                            if placeholder_new { "Start" } else { "Send" },
                            placeholder_new,
                            &theme,
                            cx.listener(move |this, _, window, cx| {
                                if can_type {
                                    this.submit(&Submit, window, cx)
                                }
                            }),
                        )
                        .debug_selector(|| "agent-submit".into()),
                    ),
            )
    }
}
