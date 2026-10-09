//! One agent task: its worktree, its conversation with the model, the plan
//! and what it did. The loop asks the model for a step, runs the tools it
//! calls, and goes again, pausing for the plan to be approved and for any
//! command that needs more than the sandbox.

use std::sync::Arc;

use ai::tools::{StepRequest, Stop, ToolCall, ToolResult, Turn};
use gpui::{Context, Entity, EventEmitter, SharedString, Task};

use crate::{
    agent::{self, Access, Change, Outcome, Phase, PlanStep, Sandbox, ToolBox, Worktree},
    ai_providers::{LOCAL, ModelRef},
    ai_store::AiStore,
    mcp_store::{self, McpStore, McpTool},
};

/// Steps a task may take before it stops and asks.
pub const MAX_STEPS: usize = 60;
/// Tool results older than this many steps are shortened in what is sent,
/// so long tasks fit a local model's context.
const KEEP_FULL: usize = 6;
const OLD_RESULT: usize = 400;

const SYSTEM: &str = "You are a coding agent in Solder, working on your own Git worktree of the \
user's project; paths are relative to its root. First explore with list_files, read_file and \
search, then propose a short plan with update_plan and stop: the user approves it before you \
change anything. Once approved, carry it out: edit with edit_file or write_file, check your work \
with run (tests, builds, linters) and fix what fails, keep the plan current with update_plan, and \
call finish with a short summary. Commands have no network and can only write inside the project; \
ask for access only when a command truly needs it. Do not commit; Solder handles Git.";

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Starting,
    Thinking,
    /// The plan is proposed; nothing changes until it is approved.
    AwaitingPlan,
    AwaitingApproval {
        command: String,
        access: Access,
        reason: String,
    },
    Finished,
    Stopped,
    Failed(SharedString),
    Merged,
    Discarded,
}

impl Status {
    pub fn busy(&self) -> bool {
        matches!(self, Status::Starting | Status::Thinking)
    }
}

#[derive(Clone, Debug)]
pub enum Entry {
    /// The model's own words.
    Text(String),
    Tool {
        /// `Read src/lib.rs`, `Ran cargo test`.
        title: String,
        output: String,
        error: bool,
        open: bool,
    },
    /// The user's instructions and Solder's notes.
    Note(String),
}

pub enum AgentEvent {
    Changed,
}

impl EventEmitter<AgentEvent> for AgentTask {}

pub struct AgentTask {
    pub title: String,
    pub model: ModelRef,
    pub status: Status,
    pub phase: Phase,
    pub plan: Vec<PlanStep>,
    pub entries: Vec<Entry>,
    pub worktree: Option<Worktree>,
    pub changes: Vec<Change>,
    pub summary: Option<String>,
    pub steps: usize,
    pub tokens: u64,
    store: Entity<AiStore>,
    sandbox: Sandbox,
    turns: Vec<Turn>,
    /// The tools of the context servers that run, as of the last step.
    mcp: Vec<McpTool>,
    /// Those of them the user let this task call. Each asks once: a
    /// server's tool runs outside the sandbox.
    mcp_allowed: Vec<String>,
    /// Calls left in the current step while one waits for the user.
    pending: Vec<ToolCall>,
    results: Vec<ToolResult>,
    task: Option<Task<()>>,
}

fn describe(call: &ToolCall) -> String {
    let s = |k: &str| call.input[k].as_str().unwrap_or("").to_string();
    match call.name.as_str() {
        "list_files" => format!(
            "Listed {}",
            if s("path").is_empty() {
                ".".into()
            } else {
                s("path")
            }
        ),
        "read_file" => format!("Read {}", s("path")),
        "search" => format!("Searched for {}", s("pattern")),
        "write_file" => format!("Wrote {}", s("path")),
        "edit_file" => format!("Edited {}", s("path")),
        "run" => format!("Ran {}", s("command")),
        "update_plan" => "Updated the plan".into(),
        "finish" => "Finished".into(),
        other => format!("Called {other}"),
    }
}

impl AgentTask {
    /// Starts a task: a worktree from the last commit, then planning.
    pub fn start(
        title: String,
        model: ModelRef,
        repo: std::path::PathBuf,
        trees: std::path::PathBuf,
        store: Entity<AiStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            title: title.clone(),
            model,
            status: Status::Starting,
            phase: Phase::Planning,
            plan: Vec::new(),
            entries: vec![Entry::Note(title.clone())],
            worktree: None,
            changes: Vec::new(),
            summary: None,
            steps: 0,
            tokens: 0,
            store,
            sandbox: Sandbox::None,
            turns: vec![Turn::User(format!("Task: {title}"))],
            mcp: Vec::new(),
            mcp_allowed: Vec::new(),
            pending: Vec::new(),
            results: Vec::new(),
            task: None,
        };
        let created = cx.background_executor().spawn(async move {
            let sandbox = Sandbox::detect();
            Worktree::create(&repo, &trees, &title).map(|wt| (wt, sandbox))
        });
        this.task = Some(cx.spawn(async move |this, cx| {
            let created = created.await;
            this.update(cx, |this, cx| match created {
                Ok((wt, sandbox)) => {
                    this.entries.push(Entry::Note(format!(
                        "Working on branch {} from the last commit{}.",
                        wt.branch,
                        if sandbox == Sandbox::None {
                            "; there is no sandbox here, so every command asks first"
                        } else {
                            ""
                        }
                    )));
                    this.worktree = Some(wt);
                    this.sandbox = sandbox;
                    this.connect(cx);
                }
                Err(e) => this.fail(e, cx),
            })
            .ok();
        }));
        this
    }

    /// Starts the context servers of the settings, then takes the first
    /// step. With none configured this is no wait at all; one that does
    /// not start is said, and the task goes on without it.
    fn connect(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.worktree.as_ref().map(|wt| wt.path.clone()) else {
            return self.next(cx);
        };
        let store = McpStore::global(cx);
        let ready = store.update(cx, |store, cx| store.start_all(&root, cx));
        self.task = Some(cx.spawn(async move |this, cx| {
            ready.await;
            this.update(cx, |this, cx| {
                for (name, why) in store.read(cx).failures() {
                    this.entries.push(Entry::Note(format!(
                        "The context server {name} did not start: {why}"
                    )));
                }
                this.next(cx);
            })
            .ok();
        }));
    }

    /// What a call is called in the list of what the agent did.
    fn title(&self, call: &ToolCall) -> String {
        match self.mcp.iter().find(|tool| tool.spec.name == call.name) {
            Some(tool) => format!("Called {} of {}", tool.tool, tool.server_name),
            None => describe(call),
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(AgentEvent::Changed);
        cx.notify();
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.task = None;
        self.status = Status::Failed(error.into());
        self.changed(cx);
    }

    fn toolbox(&self) -> Option<Arc<ToolBox>> {
        Some(Arc::new(ToolBox {
            root: self.worktree.as_ref()?.path.clone(),
            sandbox: self.sandbox,
        }))
    }

    /// What is sent: the conversation, with old tool results shortened.
    fn request(&self) -> StepRequest {
        let result_turns = self
            .turns
            .iter()
            .filter(|t| matches!(t, Turn::Results(_)))
            .count();
        let mut seen = 0;
        let turns = self
            .turns
            .iter()
            .map(|turn| match turn {
                Turn::Results(results) => {
                    seen += 1;
                    if result_turns - seen < KEEP_FULL {
                        return turn.clone();
                    }
                    Turn::Results(
                        results
                            .iter()
                            .map(|r| {
                                let mut r = r.clone();
                                if r.output.len() > OLD_RESULT {
                                    let cut = (0..=OLD_RESULT)
                                        .rev()
                                        .find(|&i| r.output.is_char_boundary(i))
                                        .unwrap_or(0);
                                    r.output =
                                        format!("{}\n[older output shortened]", &r.output[..cut]);
                                }
                                r
                            })
                            .collect(),
                    )
                }
                other => other.clone(),
            })
            .collect();
        StepRequest {
            model: if self.model.provider == LOCAL {
                "local".into()
            } else {
                self.model.model.clone()
            },
            system: self.system(),
            turns,
            tools: agent::specs(self.phase)
                .into_iter()
                .chain(self.mcp.iter().map(|tool| tool.spec.clone()))
                .collect(),
            max_tokens: 8192,
        }
    }

    /// The standing instructions: the agent's own, then what each context
    /// server says about using it.
    fn system(&self) -> String {
        let mut system = SYSTEM.to_string();
        let mut said: Vec<&str> = Vec::new();
        for tool in &self.mcp {
            if said.contains(&tool.server_name.as_str()) {
                continue;
            }
            said.push(&tool.server_name);
            if let Some(instructions) = &tool.server.instructions {
                system.push_str(&format!(
                    "\n\nThe tools named mcp_... come from context servers and reach outside \
                     the project. About {}: {instructions}",
                    tool.server_name
                ));
            }
        }
        system
    }

    /// Asks the model for its next step.
    fn next(&mut self, cx: &mut Context<Self>) {
        self.mcp = McpStore::global(cx).read(cx).tools();
        if self.steps >= MAX_STEPS {
            self.entries.push(Entry::Note(format!(
                "Stopped after {MAX_STEPS} steps. Say how to continue, or review what changed."
            )));
            self.status = Status::Stopped;
            self.refresh_changes(cx);
            self.changed(cx);
            return;
        }
        self.steps += 1;
        self.status = Status::Thinking;
        self.changed(cx);
        let endpoint = self.store.update(cx, |s, cx| s.endpoint(&self.model, cx));
        let request = self.request();
        self.task = Some(cx.spawn(async move |this, cx| {
            let step = async {
                let endpoint = endpoint.await?;
                ai::spawn(ai::tools::step(endpoint, request)).await
            }
            .await;
            this.update(cx, |this, cx| {
                this.store.update(cx, |s, _| s.touch(&this.model));
                match step {
                    Ok(step) => {
                        this.tokens += step.input_tokens + step.output_tokens;
                        if !step.text.is_empty() {
                            this.entries.push(Entry::Text(step.text.clone()));
                        }
                        this.turns.push(Turn::Assistant {
                            text: step.text.clone(),
                            calls: step.calls.clone(),
                        });
                        if step.calls.is_empty() {
                            this.answered(step.stop, step.text, cx);
                        } else {
                            this.pending = step.calls;
                            this.results.clear();
                            this.run_pending(false, cx);
                        }
                    }
                    Err(e) => this.fail(e, cx),
                }
            })
            .ok();
        }));
    }

    /// The model answered without calling a tool.
    fn answered(&mut self, stop: Stop, text: String, cx: &mut Context<Self>) {
        match (self.phase, stop) {
            (_, Stop::Length) => {
                self.turns.push(Turn::User(
                    "Your answer was cut off. Continue, in smaller steps.".into(),
                ));
                self.next(cx);
            }
            (Phase::Planning, _) if !self.plan.is_empty() => {
                self.task = None;
                self.status = Status::AwaitingPlan;
                self.changed(cx);
            }
            (Phase::Planning, _) => {
                self.turns.push(Turn::User(
                    "Propose your plan with the update_plan tool now.".into(),
                ));
                self.next(cx);
            }
            (Phase::Working, _) => {
                // Done without calling finish: take its words as the summary.
                self.finish(text, cx);
            }
        }
    }

    /// Runs the step's remaining calls in order, stopping at one that
    /// needs the user.
    fn run_pending(&mut self, approved_first: bool, cx: &mut Context<Self>) {
        let Some(toolbox) = self.toolbox() else {
            return self.fail("The worktree is gone".into(), cx);
        };
        let calls = std::mem::take(&mut self.pending);
        let phase = self.phase;
        // The user's yes to a tool of a context server holds for the task.
        if approved_first
            && let Some(first) = calls.first()
            && self.mcp.iter().any(|tool| tool.spec.name == first.name)
            && !self.mcp_allowed.contains(&first.name)
        {
            self.mcp_allowed.push(first.name.clone());
        }
        let (mcp, allowed) = (self.mcp.clone(), self.mcp_allowed.clone());
        self.status = Status::Thinking;
        self.changed(cx);
        self.task = Some(cx.spawn(async move |this, cx| {
            let mut approved = approved_first;
            let mut calls = calls.into_iter();
            while let Some(call) = calls.next() {
                let tb = toolbox.clone();
                let c = call.clone();
                let tool = mcp.iter().find(|tool| tool.spec.name == c.name).cloned();
                let outcome = match tool {
                    // A context server's tool: it runs where the server
                    // does, so the user is asked the first time.
                    Some(tool) if !approved && !allowed.contains(&c.name) => {
                        Outcome::NeedsApproval {
                            command: tool.shown(&c.input),
                            access: Access::Full,
                            reason: format!(
                                "{} is a context server. Allowing this lets the task call this tool again without asking.",
                                tool.server_name
                            ),
                        }
                    }
                    Some(tool) => {
                        cx.background_executor()
                            .spawn(async move {
                                let (output, error) = tool
                                    .server
                                    .call(&tool.tool, c.input.clone(), mcp_store::CALL)
                                    .unwrap_or_else(|error| (error, true));
                                Outcome::Result(ToolResult {
                                    id: c.id,
                                    output,
                                    error,
                                })
                            })
                            .await
                    }
                    None => {
                        cx.background_executor()
                            .spawn(async move { tb.execute(&c, phase, approved) })
                            .await
                    }
                };
                approved = false;
                let go_on = this
                    .update(cx, |this, cx| {
                        this.outcome(call, outcome, calls.as_slice(), cx)
                    })
                    .unwrap_or(false);
                if !go_on {
                    return;
                }
            }
            this.update(cx, |this, cx| this.step_done(cx)).ok();
        }));
    }

    /// Records one call's outcome; false when the loop must wait or stop.
    fn outcome(
        &mut self,
        call: ToolCall,
        outcome: Outcome,
        rest: &[ToolCall],
        cx: &mut Context<Self>,
    ) -> bool {
        match outcome {
            Outcome::Result(result) => {
                self.entries.push(Entry::Tool {
                    title: self.title(&call),
                    output: result.output.clone(),
                    error: result.error,
                    open: false,
                });
                self.results.push(result);
                self.changed(cx);
                true
            }
            Outcome::Plan(steps, result) => {
                self.plan = steps;
                self.results.push(result);
                self.changed(cx);
                true
            }
            Outcome::NeedsApproval {
                command,
                access,
                reason,
            } => {
                self.pending = std::iter::once(call).chain(rest.iter().cloned()).collect();
                self.task = None;
                self.status = Status::AwaitingApproval {
                    command,
                    access,
                    reason,
                };
                self.changed(cx);
                false
            }
            Outcome::Finished(summary) => {
                self.results.push(ToolResult {
                    id: call.id,
                    output: "Done.".into(),
                    error: false,
                });
                self.turns
                    .push(Turn::Results(std::mem::take(&mut self.results)));
                self.finish(summary, cx);
                false
            }
        }
    }

    fn step_done(&mut self, cx: &mut Context<Self>) {
        self.turns
            .push(Turn::Results(std::mem::take(&mut self.results)));
        if self.phase == Phase::Planning && !self.plan.is_empty() {
            self.task = None;
            self.status = Status::AwaitingPlan;
            self.changed(cx);
        } else {
            self.next(cx);
        }
    }

    fn finish(&mut self, summary: String, cx: &mut Context<Self>) {
        self.task = None;
        self.status = Status::Finished;
        for step in &mut self.plan {
            step.done = true;
        }
        self.summary = Some(summary);
        self.refresh_changes(cx);
        self.changed(cx);
    }

    pub fn refresh_changes(&mut self, cx: &mut Context<Self>) {
        let Some(wt) = self.worktree.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let changes = cx
                .background_executor()
                .spawn(async move { wt.changes() })
                .await;
            this.update(cx, |this, cx| {
                this.changes = changes.unwrap_or_default();
                this.changed(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The user approved the plan, as edited; work begins.
    pub fn approve_plan(&mut self, steps: Vec<String>, cx: &mut Context<Self>) {
        if self.status != Status::AwaitingPlan {
            return;
        }
        let steps: Vec<String> = steps
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if steps.is_empty() {
            return;
        }
        self.plan = steps
            .iter()
            .map(|text| PlanStep {
                text: text.clone(),
                done: false,
            })
            .collect();
        let list: String = steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {s}\n", i + 1))
            .collect();
        self.turns.push(Turn::User(format!(
            "The plan is approved:\n{list}Carry it out now. Mark steps done with update_plan and call finish when the work is checked."
        )));
        self.entries.push(Entry::Note("Plan approved.".into()));
        self.phase = Phase::Working;
        self.next(cx);
    }

    /// The user allowed or refused the command waiting for them.
    pub fn decide(&mut self, allow: bool, cx: &mut Context<Self>) {
        let Status::AwaitingApproval { command, .. } = self.status.clone() else {
            return;
        };
        if allow {
            self.run_pending(true, cx);
        } else {
            let mut calls = std::mem::take(&mut self.pending).into_iter();
            if let Some(call) = calls.next() {
                self.entries.push(Entry::Tool {
                    title: self.title(&call),
                    output: "Declined by the user.".into(),
                    error: true,
                    open: false,
                });
                self.results.push(ToolResult {
                    id: call.id,
                    output: format!("The user declined to run `{command}`. Find another way, or explain what is needed."),
                    error: true,
                });
            }
            self.pending = calls.collect();
            self.run_pending(false, cx);
        }
    }

    /// Gives every call of the last step a result, so the conversation
    /// stays valid after a stop or new instructions mid-step.
    fn close_open_calls(&mut self, why: &str) {
        let mut results = std::mem::take(&mut self.results);
        self.pending.clear();
        let Some(Turn::Assistant { calls, .. }) = self.turns.last() else {
            return;
        };
        for call in calls {
            if !results.iter().any(|r| r.id == call.id) {
                results.push(ToolResult {
                    id: call.id.clone(),
                    output: why.to_string(),
                    error: true,
                });
            }
        }
        if !calls.is_empty() {
            self.turns.push(Turn::Results(results));
        }
    }

    /// More instructions after a pause or the end: work goes on.
    pub fn follow_up(&mut self, text: String, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() || self.status.busy() || self.worktree.is_none() {
            return;
        }
        if matches!(self.status, Status::Merged | Status::Discarded) {
            return;
        }
        // A new instruction declines a waiting command.
        self.close_open_calls("Not run: the user gave new instructions.");
        self.entries.push(Entry::Note(text.clone()));
        self.turns.push(Turn::User(text));
        if self.status == Status::AwaitingPlan {
            // Revise the plan rather than start.
            self.plan.clear();
        } else {
            self.phase = Phase::Working;
        }
        self.summary = None;
        self.next(cx);
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if self.task.take().is_some() || matches!(self.status, Status::AwaitingApproval { .. }) {
            self.close_open_calls("Not run: the user stopped the task.");
            self.status = Status::Stopped;
            self.entries.push(Entry::Note("Stopped.".into()));
            self.refresh_changes(cx);
            self.changed(cx);
        }
    }

    /// Merges the work into the user's branch and removes the worktree.
    pub fn merge(&mut self, cx: &mut Context<Self>) {
        let Some(wt) = self.worktree.clone() else {
            return;
        };
        if self.status.busy() {
            return;
        }
        let message = format!("{}\n\nDone by an agent in Solder.", self.title);
        self.status = Status::Thinking;
        self.changed(cx);
        self.task = Some(cx.spawn(async move |this, cx| {
            let merged = cx
                .background_executor()
                .spawn(async move { wt.merge(&message) })
                .await;
            this.update(cx, |this, cx| {
                this.task = None;
                match merged {
                    Ok(()) => {
                        this.status = Status::Merged;
                        this.entries
                            .push(Entry::Note("Merged into your branch.".into()));
                    }
                    Err(e) => {
                        this.status = Status::Finished;
                        this.entries.push(Entry::Note(e));
                    }
                }
                this.changed(cx);
            })
            .ok();
        }));
    }

    /// Throws the work away: worktree and branch.
    pub fn discard(&mut self, cx: &mut Context<Self>) {
        let Some(wt) = self.worktree.clone() else {
            return;
        };
        self.task = None;
        self.status = Status::Discarded;
        self.entries.push(Entry::Note("Discarded.".into()));
        cx.background_executor()
            .spawn(async move { wt.remove() })
            .detach();
        self.changed(cx);
    }

    pub fn toggle_entry(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(Entry::Tool { open, .. }) = self.entries.get_mut(ix) {
            *open = !*open;
            self.changed(cx);
        }
    }

    /// Before and after for a changed file, for review.
    pub fn diff_texts(&self, rel: &str, cx: &mut Context<Self>) -> Task<(String, String)> {
        let Some(wt) = self.worktree.clone() else {
            return Task::ready((String::new(), String::new()));
        };
        let rel = rel.to_string();
        cx.background_executor().spawn(async move {
            (
                wt.file_at_base(&rel).unwrap_or_default(),
                wt.file_now(&rel).unwrap_or_default(),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_calls() {
        let call = |name: &str, input: serde_json::Value| ToolCall {
            id: "x".into(),
            name: name.into(),
            input,
        };
        assert_eq!(
            describe(&call("read_file", serde_json::json!({"path": "a.rs"}))),
            "Read a.rs"
        );
        assert_eq!(
            describe(&call("run", serde_json::json!({"command": "cargo test"}))),
            "Ran cargo test"
        );
        assert_eq!(
            describe(&call("list_files", serde_json::json!({}))),
            "Listed ."
        );
    }
}
