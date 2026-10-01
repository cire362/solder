//! Code suggested at the cursor while typing, drawn as faint "ghost" text
//! and streamed as the model writes it. Tab inserts it in one undo step,
//! Escape dismisses it; typing what it suggests keeps the rest.
//!
//! The model is the one chosen for completions in the AI tab. Files the
//! project keeps from AI (`.env`, `.solderignore`) are never sent.

use std::time::Duration;

use ai::{
    Role,
    complete::{self, FillRequest, Mode, NO_INFILL},
};
use gpui::{Context, Window};

use crate::{
    ai_context,
    ai_providers::LOCAL,
    ai_store::AiStore,
    editor::{AcceptGhost, DismissGhost, Editor},
};

/// Wait this long after the last keystroke before asking.
const DELAY: Duration = Duration::from_millis(350);
/// Context sent around the cursor, in bytes.
const PREFIX_BYTES: usize = 6000;
const SUFFIX_BYTES: usize = 2000;

pub struct Ghost {
    pub offset: usize,
    pub text: String,
}

fn floor_boundary(text: &str, mut i: usize) -> usize {
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(text: &str, mut i: usize) -> usize {
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

impl Editor {
    /// A suggestion to draw and accept: not while the completion menu is up.
    pub(crate) fn showing_ghost(&self) -> bool {
        self.completion.is_none() && self.ghost.as_ref().is_some_and(|g| !g.text.is_empty())
    }

    pub(crate) fn clear_ghost(&mut self, cx: &mut Context<Self>) {
        self.ghost_task = None;
        if self.ghost.take().is_some() {
            cx.notify();
        }
    }

    /// After the cursor moved or text changed: keeps the suggestion when
    /// the text typed since is its start, else drops it.
    pub(crate) fn sync_ghost(&mut self, cx: &mut Context<Self>) {
        let Some(ghost) = &self.ghost else {
            return;
        };
        let single = self.selections.len() == 1 && self.selections[0].is_empty();
        let head = self.selections[0].head;
        if !single || head < ghost.offset {
            self.clear_ghost(cx);
            return;
        }
        let typed = self.buf(cx).text_for_range(ghost.offset..head);
        let Some(ghost) = self.ghost.as_mut() else {
            return;
        };
        if ghost.text.starts_with(typed.as_str()) {
            ghost.text.drain(..typed.len());
            ghost.offset = head;
            // An empty suggestion still streaming stays, to be filled.
            if ghost.text.is_empty() && self.ghost_task.is_none() {
                self.ghost = None;
            }
            cx.notify();
        } else {
            self.clear_ghost(cx);
        }
    }

    /// Asks for a suggestion once typing pauses, when a model has the task,
    /// the cursor ends its line and the file may be shared.
    pub(crate) fn schedule_ghost(&mut self, cx: &mut Context<Self>) {
        if self.ghost.is_some()
            || self.is_single_line()
            || self.selections.len() != 1
            || !self.selections[0].is_empty()
        {
            return;
        }
        let Some(store) = AiStore::try_global(cx) else {
            return;
        };
        let (model, root) = {
            let s = store.read(cx);
            let Some(model) = s.roles.get(&Role::Completion).cloned() else {
                return;
            };
            if !s.completions {
                return;
            }
            let Some(path) = self.path(cx) else {
                return;
            };
            (model, s.root_for(path))
        };
        let Some(path) = self.path(cx).map(|p| p.to_path_buf()) else {
            return;
        };
        let head = self.selections[0].head;
        let text = self.text(cx);
        let line_end = text[head..].find('\n').map_or(text.len(), |i| head + i);
        if !text[head..line_end].trim().is_empty() {
            return;
        }
        let start = floor_boundary(&text, head.saturating_sub(PREFIX_BYTES));
        let end = ceil_boundary(&text, (head + SUFFIX_BYTES).min(text.len()));
        let shown = root
            .as_ref()
            .and_then(|r| path.strip_prefix(r).ok())
            .unwrap_or(&path)
            .display()
            .to_string();
        let request = FillRequest {
            // llama.cpp serves the one model it loaded, whatever the name.
            model: if model.provider == LOCAL {
                "local".into()
            } else {
                model.model.clone()
            },
            path: shown,
            prefix: text[start..head].to_string(),
            suffix: text[head..end].to_string(),
            max_tokens: 128,
        };
        let version = self.version(cx);
        self.ghost_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DELAY).await;
            let still = this
                .update(cx, |e, cx| {
                    e.version(cx) == version && e.newest_range() == (head..head)
                })
                .unwrap_or(false);
            if !still {
                return;
            }
            let allowed = cx
                .background_executor()
                .spawn(async move {
                    match root {
                        Some(root) => ai_context::allows_path(&root, &path).unwrap_or(false),
                        None => ai::context::allows_file(&path),
                    }
                })
                .await;
            if !allowed {
                return;
            }
            let Ok(endpoint) = store.update(cx, |s, cx| s.endpoint(&model, cx)) else {
                return;
            };
            let Ok(endpoint) = endpoint.await else {
                return;
            };
            let Ok(mut mode) = store.read_with(cx, |s, _| {
                if s.no_infill.contains(&model.model) {
                    Mode::Chat
                } else {
                    complete::mode_for(model.provider == LOCAL, endpoint.api)
                }
            }) else {
                return;
            };
            let ready = this
                .update(cx, |e, cx| {
                    let fresh = e.version(cx) == version && e.newest_range() == (head..head);
                    if fresh {
                        e.ghost = Some(Ghost {
                            offset: head,
                            text: String::new(),
                        });
                    }
                    fresh
                })
                .unwrap_or(false);
            if !ready {
                return;
            }
            'ask: loop {
                let mut stream = complete::stream(endpoint.clone(), mode, request.clone());
                while let Some(item) = stream.recv().await {
                    match item {
                        Ok(text) => {
                            let alive = this
                                .update(cx, |e, cx| {
                                    let Some(ghost) = e.ghost.as_mut() else {
                                        return false;
                                    };
                                    ghost.text.push_str(&text);
                                    store.update(cx, |s, _| s.touch_role(Role::Completion));
                                    cx.notify();
                                    true
                                })
                                .unwrap_or(false);
                            if !alive {
                                return;
                            }
                        }
                        Err(e) if e == NO_INFILL && mode == Mode::Infill => {
                            // Remembered, so later requests go straight to chat.
                            store
                                .update(cx, |s, _| s.no_infill.insert(model.model.clone()))
                                .ok();
                            mode = Mode::Chat;
                            continue 'ask;
                        }
                        Err(_) => break,
                    }
                }
                break;
            }
            this.update(cx, |e, cx| {
                e.ghost_task = None;
                // Nothing to add: blank, or only what already follows.
                let useless = e.ghost.as_ref().is_some_and(|g| {
                    let text = g.text.trim();
                    text.is_empty() || request.suffix.trim_start().starts_with(text)
                });
                if useless {
                    e.ghost = None;
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn accept_ghost(&mut self, _: &AcceptGhost, _: &mut Window, cx: &mut Context<Self>) {
        if !self.showing_ghost() {
            cx.propagate();
            return;
        }
        let Some(ghost) = self.ghost.take() else {
            return;
        };
        self.ghost_task = None;
        if self.selections.len() == 1 && self.selections[0].head == ghost.offset {
            // Its own undo step, apart from the typing before and after.
            self.document.update(cx, |d, _| d.seal_history());
            self.insert(&ghost.text, cx);
            self.document.update(cx, |d, _| d.seal_history());
        }
        cx.notify();
    }

    pub(crate) fn dismiss_ghost(
        &mut self,
        _: &DismissGhost,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ghost.is_none() && self.ghost_task.is_none() {
            cx.propagate();
            return;
        }
        self.clear_ghost(cx);
    }
}
