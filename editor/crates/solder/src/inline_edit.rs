use std::{
    ops::Range,
    path::PathBuf,
    time::{Duration, Instant},
};

use ai::{Role, edit, provider::Event};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, SharedString,
    Subscription, Task, WeakEntity, Window, actions, div, prelude::*, px,
};
use text::Rope;

use crate::{
    ai_context::{self, Cancellation},
    ai_providers::LOCAL,
    ai_store::AiStore,
    chat_panel::{FILE_LIMIT, FileContext},
    editor::Editor,
    file_diff::{DiffModel, FileDiff, FileDiffEvent},
    git::FileDiffSnapshot,
    settings::Settings,
    theme::{ActiveTheme, CODE_FONT, UI_FONT_SIZE},
    ui,
};

actions!(inline_edit, [Generate, Apply, Cancel]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", Generate, Some("InlineEditInput")),
        KeyBinding::new("secondary-enter", Apply, Some("InlineEdit")),
        KeyBinding::new("escape", Cancel, Some("InlineEdit")),
    ]);
}

pub enum InlineEditEvent {
    Close,
    ChooseModel,
}

#[derive(Clone)]
struct Snapshot {
    source: PathBuf,
    path: String,
    version: u64,
    range: Range<usize>,
    rope: Rope,
}

impl Snapshot {
    fn capture(editor: &Editor, root: &std::path::Path, cx: &App) -> Result<Self, String> {
        if editor.doc(cx).is_read_only() {
            return Err("This file is read-only.".into());
        }
        if editor.cursor_position(cx).2 != 1 {
            return Err("Use one selection for an inline edit.".into());
        }
        let source = editor
            .path(cx)
            .ok_or("Save this file before using AI edits.")?
            .to_path_buf();
        let path = source
            .strip_prefix(root)
            .map_err(|_| "Open a file inside this project.")?
            .to_string_lossy()
            .replace('\\', "/");
        let rope = editor.rope(cx).clone();
        let selected = editor.newest_range();
        let range = if selected.is_empty() {
            0..rope.len_bytes()
        } else {
            selected
        };
        if rope.byte_slice(range.clone()).len_chars() > FILE_LIMIT {
            return Err("Select a smaller part of the file (up to 24,000 characters).".into());
        }
        Ok(Self {
            source,
            path,
            version: editor.version(cx),
            range,
            rope,
        })
    }

    fn context(&self, root: PathBuf, project: bool, question: String) -> ai_context::Input {
        ai_context::Input {
            root,
            file: Some(FileContext {
                source: self.source.clone(),
                path: self.path.clone(),
                text: String::new(),
                part: None,
            }),
            project,
            question,
            history: Vec::new(),
        }
    }

    fn text_context(&self, map: Option<String>) -> edit::Context {
        let start = self.rope.byte_to_char(self.range.start);
        let end = self.rope.byte_to_char(self.range.end);
        edit::Context {
            path: self.path.clone(),
            before: self
                .rope
                .slice(start.saturating_sub(2_000)..start)
                .to_string(),
            target: self.rope.slice(start..end).to_string(),
            after: self
                .rope
                .slice(end..(end + 2_000).min(self.rope.len_chars()))
                .to_string(),
            map,
        }
    }
}

pub struct InlineEdit {
    target: WeakEntity<Editor>,
    root: PathBuf,
    store: Entity<AiStore>,
    input: Entity<Editor>,
    focus: FocusHandle,
    snapshot: Option<Snapshot>,
    proposal: Option<String>,
    diff: Option<Entity<FileDiff>>,
    lines: Vec<SharedString>,
    include_project: bool,
    task: Option<Task<()>>,
    status: String,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<InlineEditEvent> for InlineEdit {}

impl InlineEdit {
    pub fn new(
        target: Entity<Editor>,
        root: PathBuf,
        store: Entity<AiStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let document = target.read(cx).document().clone();
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&document, |this, _, _, cx| {
                if this.snapshot.is_some() && !this.current(cx) {
                    this.fail("File changed. Generate again.".into(), cx);
                }
            }),
        ];
        store.update(cx, |store, cx| store.load(cx));
        let selected = !target.read(cx).newest_range().is_empty();
        Self {
            target: target.downgrade(),
            root,
            store,
            input: cx.new(|cx| Editor::single_line("Describe the change", cx)),
            focus: cx.focus_handle(),
            snapshot: None,
            proposal: None,
            diff: None,
            lines: Vec::new(),
            include_project: false,
            task: None,
            status: if selected {
                "Edit selection"
            } else {
                "Edit whole file"
            }
            .into(),
            error: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn input(&self) -> Entity<Editor> {
        self.input.clone()
    }

    #[cfg(test)]
    pub fn ready(&self) -> bool {
        self.task.is_none() && self.proposal.is_some()
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub fn has_streamed_text(&self) -> bool {
        !self.lines.is_empty()
    }

    #[cfg(test)]
    pub fn streaming(&self) -> bool {
        self.task.is_some()
    }

    #[cfg(test)]
    pub fn previewed(&self) -> bool {
        self.diff.is_some() && self.task.is_none()
    }

    pub fn abort(&mut self, cx: &mut Context<Self>) {
        self.task = None;
        self.snapshot = None;
        self.proposal = None;
        self.status = "Stopped. Generate again to retry.".into();
        cx.notify();
    }

    pub fn is_target(&self, editor: &Entity<Editor>) -> bool {
        self.target.entity_id() == editor.entity_id()
    }

    fn current(&self, cx: &App) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        self.target.upgrade().is_some_and(|target| {
            let editor = target.read(cx);
            editor.version(cx) == snapshot.version
                && editor.path(cx) == Some(snapshot.source.as_path())
                && !editor.doc(cx).is_read_only()
        })
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.task = None;
        self.snapshot = None;
        self.proposal = None;
        self.error = Some(error);
        cx.notify();
    }

    pub fn generate(&mut self, _: &Generate, _: &mut Window, cx: &mut Context<Self>) {
        let instruction = self.input.read(cx).text(cx);
        if self.task.is_some() || instruction.trim().is_empty() {
            return;
        }
        let Some(model) = self.store.read(cx).roles.get(&Role::Chat).cloned() else {
            self.fail("Choose a chat model first.".into(), cx);
            return;
        };
        let Some(target) = self.target.upgrade() else {
            return;
        };
        let snapshot = match Snapshot::capture(target.read(cx), &self.root, cx) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.fail(error, cx);
                return;
            }
        };
        self.status = format!("Preparing {}...", snapshot.path);
        self.error = None;
        self.proposal = None;
        self.diff = None;
        self.lines.clear();
        self.snapshot = Some(snapshot.clone());
        let context =
            snapshot.context(self.root.clone(), self.include_project, instruction.clone());
        let endpoint = self
            .store
            .update(cx, |store, cx| store.endpoint(&model, cx));
        let executor = cx.background_executor().clone();
        let cancellation = Cancellation::new();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let endpoint = endpoint.await?;
                let flag = cancellation.flag();
                let (request, original) = executor
                    .spawn(async move {
                        let prepared = ai_context::prepare(context, &flag)?;
                        if prepared.file.is_none() {
                            return Err("This file is excluded from AI context.".to_string());
                        }
                        let context = snapshot.text_context(prepared.map.map(|map| map.text));
                        let original = context.target.clone();
                        let model = if model.provider == LOCAL {
                            "local".into()
                        } else {
                            model.model
                        };
                        Ok((edit::request(model, context, instruction), original))
                    })
                    .await?;
                this.update(cx, |this, cx| {
                    this.status = "Generating preview...".into();
                    cx.notify();
                })
                .map_err(|error| error.to_string())?;
                let mut stream = ai::provider::stream_edit(endpoint, request);
                let mut answer = String::new();
                let mut painted = Instant::now() - Duration::from_secs(1);
                while let Some(event) = stream.recv().await {
                    if let Event::Text(text) = event? {
                        if answer.len().saturating_add(text.len()) > edit::RESPONSE_LIMIT {
                            return Err("The proposed edit is too large. Select less code.".into());
                        }
                        answer.push_str(&text);
                        if painted.elapsed() >= Duration::from_millis(80) {
                            let preview = answer.clone();
                            let lines = executor
                                .spawn(async move {
                                    preview
                                        .lines()
                                        .map(|line| SharedString::from(line.to_string()))
                                        .collect()
                                })
                                .await;
                            this.update(cx, |this, cx| {
                                this.lines = lines;
                                this.store
                                    .update(cx, |store, _| store.touch_role(Role::Chat));
                                cx.notify();
                            })
                            .map_err(|error| error.to_string())?;
                            painted = Instant::now();
                        }
                    }
                }
                let path = this
                    .update(cx, |this, _| {
                        this.snapshot
                            .as_ref()
                            .map(|snapshot| snapshot.source.clone())
                    })
                    .map_err(|error| error.to_string())?
                    .ok_or("File changed. Generate again.")?;
                let (proposal, diff, changed) = executor
                    .spawn(async move {
                        let proposal = edit::replacement(&answer, &original)?;
                        let changed = proposal != original;
                        let diff = DiffModel::new(FileDiffSnapshot {
                            path: path.to_string_lossy().into_owned(),
                            old: original,
                            new: proposal.clone(),
                            can_open: false,
                        });
                        Ok::<_, String>((proposal, diff, changed))
                    })
                    .await?;
                this.update(cx, |this, cx| {
                    if !this.current(cx) {
                        this.fail("File changed. Generate again.".into(), cx);
                        return;
                    }
                    let path = this.snapshot.as_ref().unwrap().source.clone();
                    let view = cx.new(|cx| FileDiff::preview(path, cx));
                    view.update(cx, |view, cx| view.set_result(Ok(diff), cx));
                    cx.subscribe(&view, |_, _, event, cx| {
                        if matches!(event, FileDiffEvent::Close) {
                            cx.emit(InlineEditEvent::Close);
                        }
                    })
                    .detach();
                    this.diff = Some(view);
                    this.proposal = changed.then_some(proposal);
                    this.lines.clear();
                    this.task = None;
                    this.status = if changed {
                        "Review the edit. Apply with Cmd/Ctrl+Enter."
                    } else {
                        "No changes. Try another instruction."
                    }
                    .into();
                    cx.notify();
                })
                .map_err(|error| error.to_string())?;
                Ok::<_, String>(())
            }
            .await;
            if let Err(error) = result {
                this.update(cx, |this, cx| this.fail(error, cx)).ok();
            }
        }));
        cx.notify();
    }

    pub fn apply(&mut self, _: &Apply, _: &mut Window, cx: &mut Context<Self>) {
        if self.task.is_some() || self.proposal.is_none() || !self.current(cx) {
            return;
        }
        let context =
            self.snapshot
                .as_ref()
                .unwrap()
                .context(self.root.clone(), false, String::new());
        let executor = cx.background_executor().clone();
        let cancellation = Cancellation::new();
        self.status = "Checking file...".into();
        self.task = Some(cx.spawn(async move |this, cx| {
            let flag = cancellation.flag();
            let checked = executor
                .spawn(async move {
                    let prepared = ai_context::prepare(context, &flag)?;
                    if prepared.file.is_none() {
                        return Err("This file is now excluded from AI context.".to_string());
                    }
                    Ok(())
                })
                .await;
            this.update(cx, |this, cx| {
                if let Err(error) = checked {
                    this.fail(error, cx);
                    return;
                }
                if !this.current(cx) {
                    this.fail("File changed. Generate again.".into(), cx);
                    return;
                }
                let target = this.target.upgrade().unwrap();
                let snapshot = this.snapshot.take().unwrap();
                let proposal = this.proposal.take().unwrap();
                this.task = None;
                target.update(cx, |editor, cx| {
                    editor.replace_ranges(vec![(snapshot.range, proposal)], cx)
                });
                cx.emit(InlineEditEvent::Close);
            })
            .ok();
        }));
        cx.notify();
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self.task.is_some() {
            self.abort(cx);
        } else {
            cx.emit(InlineEditEvent::Close);
        }
    }
}

impl Focusable for InlineEdit {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for InlineEdit {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let busy = self.task.is_some();
        let model = self
            .store
            .read(cx)
            .roles
            .get(&Role::Chat)
            .map(|model| self.store.read(cx).model_label(model))
            .unwrap_or_else(|| "No chat model".into());
        let ready = self.proposal.is_some() && self.current(cx) && !busy;
        div()
            .id("inline-edit")
            .debug_selector(|| "inline-edit".into())
            .key_context("InlineEdit")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::generate))
            .on_action(cx.listener(Self::apply))
            .on_action(cx.listener(Self::cancel))
            .flex_shrink()
            .min_h_0()
            .max_h_full()
            .flex()
            .flex_col()
            .mx_2()
            .my_2()
            .rounded(px(16.))
            .border_1()
            .border_color(theme.line)
            .bg(theme.bg_sunken)
            .overflow_hidden()
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .child(
                div()
                    .id("edit-controls")
                    .p_2()
                    .flex_shrink()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .child(div().flex_1().min_w_0().truncate().child("Inline edit"))
                            .when(ready, |bar| {
                                bar.child(
                                    ui::button(
                                        "edit-apply",
                                        "Apply",
                                        true,
                                        &theme,
                                        cx.listener(|this, _, window, cx| {
                                            this.apply(&Apply, window, cx)
                                        }),
                                    )
                                    .debug_selector(|| "edit-apply".into()),
                                )
                            })
                            .child(ui::button(
                                "edit-discard",
                                "Discard",
                                false,
                                &theme,
                                cx.listener(|this, _, _, cx| {
                                    this.abort(cx);
                                    cx.emit(InlineEditEvent::Close);
                                }),
                            )),
                    )
                    .child(
                        div()
                            .key_context("InlineEditInput")
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ui::text_field(
                                self.input.clone(),
                                self.input.focus_handle(cx).is_focused(window),
                                &theme,
                            ))
                            .child(ui::button(
                                "edit-generate",
                                if busy { "Stop" } else { "Generate" },
                                !ready,
                                &theme,
                                cx.listener(move |this, _, window, cx| {
                                    if busy {
                                        this.cancel(&Cancel, window, cx);
                                    } else {
                                        this.generate(&Generate, window, cx);
                                    }
                                }),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                ui::toggle(
                                    "edit-project",
                                    "Project map",
                                    "Include project declarations",
                                    self.include_project,
                                    &theme,
                                    cx.listener(|this, _, window, cx| {
                                        if this.task.is_none() {
                                            this.include_project = !this.include_project;
                                            window.focus(&this.input.focus_handle(cx));
                                            cx.notify();
                                        }
                                    }),
                                )
                                .debug_selector(|| "edit-project".into())
                                .when(!self.include_project, |toggle| {
                                    toggle.text_color(theme.fg_muted)
                                }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.fg_muted)
                                    .child(format!("Chat model: {model}")),
                            )
                            .child(ui::button(
                                "edit-models",
                                "Models",
                                false,
                                &theme,
                                cx.listener(|_, _, _, cx| cx.emit(InlineEditEvent::ChooseModel)),
                            )),
                    )
                    .child(
                        div()
                            .text_color(if self.error.is_some() {
                                theme.error
                            } else {
                                theme.fg_muted
                            })
                            .child(self.error.clone().unwrap_or_else(|| self.status.clone())),
                    )
                    .children(self.snapshot.as_ref().map(|snapshot| {
                        div().text_color(theme.fg_muted).truncate().child(format!(
                            "{} · lines {}-{}",
                            snapshot.path,
                            snapshot.rope.byte_to_line(snapshot.range.start) + 1,
                            snapshot
                                .rope
                                .byte_to_line(snapshot.range.end.saturating_sub(1))
                                + 1,
                        ))
                    })),
            )
            .when(self.diff.is_some() || !self.lines.is_empty(), |panel| {
                panel.child(
                    div()
                        .h(px(
                            (f32::from(window.viewport_size().height) * 0.3).clamp(80., 260.)
                        ))
                        .min_h_0()
                        .flex_shrink()
                        .border_t_1()
                        .border_color(theme.line)
                        .map(|body| {
                            if let Some(diff) = &self.diff {
                                body.child(diff.clone()).into_any_element()
                            } else {
                                body.child(
                                    gpui::uniform_list(
                                        "edit-stream",
                                        self.lines.len(),
                                        cx.processor(|this, range: Range<usize>, _, cx| {
                                            range
                                                .map(|index| {
                                                    div()
                                                        .h(Settings::get(cx).line_height())
                                                        .px_2()
                                                        .font_family(CODE_FONT)
                                                        .whitespace_nowrap()
                                                        .child(this.lines[index].clone())
                                                })
                                                .collect::<Vec<_>>()
                                        }),
                                    )
                                    .size_full(),
                                )
                                .into_any_element()
                            }
                        }),
                )
            })
    }
}
