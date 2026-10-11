//! Literal search of retained terminal cells. Scanning runs away from the
//! window and releases the emulator between logical lines.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use super::{Listener, Terminal};
use crate::{
    editor::{Editor, EditorEvent},
    theme::{ActiveTheme, UI_FONT_SMALL},
    ui,
};
use alacritty_terminal::{
    grid::Dimensions,
    index::{Line, Point},
    sync::FairMutex,
    term::Term,
};
use gpui::{
    App, Context, Entity, Focusable, KeyBinding, Render, Subscription, Task, Window, actions, div,
    prelude::*,
};

actions!(terminal_search, [Next, Previous, Close]);

pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", Next, Some("TerminalSearch")),
        KeyBinding::new("shift-enter", Previous, Some("TerminalSearch")),
        KeyBinding::new("escape", Close, Some("TerminalSearch")),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Match {
    pub start: Point,
    pub end: Point,
}

#[derive(Default)]
pub(super) struct Found {
    pub matches: Vec<Match>,
    pub limited: bool,
}

pub(super) struct Search {
    pub visible: bool,
    pub found: Found,
    pub active: Option<usize>,
    query: Entity<Editor>,
    terminal: gpui::WeakEntity<Terminal>,
    term: Arc<FairMutex<Term<Listener>>>,
    generation: Arc<AtomicU64>,
    pending: bool,
    task: Option<Task<()>>,
    _query: Subscription,
}

impl Search {
    pub fn new(
        terminal: gpui::WeakEntity<Terminal>,
        term: Arc<FairMutex<Term<Listener>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| Editor::single_line("Find in terminal", cx));
        let subscription = cx.subscribe(&query, |this, _, event, cx| {
            if matches!(event, EditorEvent::Edited) {
                this.refresh(true, cx);
            }
        });
        Self {
            visible: false,
            found: Found::default(),
            active: None,
            query,
            terminal,
            term,
            generation: Arc::new(AtomicU64::new(0)),
            pending: false,
            task: None,
            _query: subscription,
        }
    }

    pub fn deploy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = true;
        window.focus(&self.query.focus_handle(cx));
        self.refresh(true, cx);
    }

    pub fn refresh(&mut self, reveal: bool, cx: &mut Context<Self>) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.task = None;
        if !self.visible {
            return;
        }
        let query = self.query.read(cx).text(cx);
        let old = self
            .active
            .and_then(|i| self.found.matches.get(i).map(|m| m.start));
        self.found = Found::default();
        self.active = None;
        self.pending = !query.is_empty();
        if query.is_empty() {
            cx.notify();
            return;
        }
        let term = self.term.clone();
        let counter = self.generation.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(150))
                .await;
            let found = cx
                .background_executor()
                .spawn(async move { find(&term, &query, &counter, generation) })
                .await;
            this.update(cx, |this, cx| {
                if this.generation.load(Ordering::Relaxed) != generation || !this.visible {
                    return;
                }
                this.found = found;
                this.pending = false;
                this.active = if this.found.matches.is_empty() {
                    None
                } else {
                    Some(
                        old.and_then(|at| this.found.matches.iter().position(|m| m.start >= at))
                            .unwrap_or(0),
                    )
                };
                if reveal {
                    this.reveal();
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn reveal(&self) {
        if let Some(found) = self.active.and_then(|i| self.found.matches.get(i)) {
            self.term.lock().scroll_to_point(found.start);
        }
    }

    fn move_match(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let n = self.found.matches.len();
        if n == 0 {
            return;
        }
        let old = self.active.unwrap_or(0);
        self.active = Some(if backwards {
            (old + n - 1) % n
        } else {
            (old + 1) % n
        });
        self.reveal();
        cx.notify();
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.move_match(false, cx);
    }
    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.move_match(true, cx);
    }
    fn close(&mut self, _: &Close, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = false;
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.task = None;
        self.found = Found::default();
        self.active = None;
        if let Some(terminal) = self.terminal.upgrade() {
            window.focus(&terminal.focus_handle(cx));
        }
        cx.notify();
    }

    pub fn highlight(&self, point: Point) -> Option<bool> {
        let i = self
            .found
            .matches
            .partition_point(|m| m.start <= point)
            .checked_sub(1)?;
        (point <= self.found.matches[i].end).then_some(self.active == Some(i))
    }
}

fn find(
    term: &Arc<FairMutex<Term<Listener>>>,
    query: &str,
    counter: &AtomicU64,
    generation: u64,
) -> Found {
    let mut found = Found::default();
    if query.is_empty() {
        return found;
    }
    let (mut row, bottom) = {
        let term = term.lock();
        (term.topmost_line().0, term.bottommost_line().0)
    };
    let mut bytes = 0;
    while row <= bottom {
        if counter.load(Ordering::Relaxed) != generation {
            return Found::default();
        }
        let line = {
            let term = term.lock();
            if row < term.topmost_line().0 || bottom != term.bottommost_line().0 {
                return Found::default();
            }
            crate::terminal_output::read_from(&*term, Line(row))
        };
        bytes += line.text.len();
        found.limited |= line.limited;
        for (byte, text) in line.text.match_indices(query) {
            let start = line
                .cells
                .iter()
                .find(|(r, _)| r.contains(&byte))
                .map(|(_, p)| *p);
            let end = line
                .cells
                .iter()
                .rev()
                .find(|(r, _)| r.contains(&(byte + text.len() - 1)))
                .map(|(_, p)| *p);
            if let (Some(start), Some(end)) = (start, end) {
                found.matches.push(Match { start, end });
            }
            if found.matches.len() > 20_000 {
                found.matches.pop();
                found.limited = true;
                return found;
            }
        }
        if bytes >= 8 * 1024 * 1024 {
            found.limited = true;
            break;
        }
        row = line.last_row.0 + 1;
    }
    found
}

impl Render for Search {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let focused = self.query.focus_handle(cx).is_focused(window);
        let status: String = if self.pending {
            "Searching...".into()
        } else {
            format!(
                "{} / {}{}",
                self.active.map_or(0, |i| i + 1),
                self.found.matches.len(),
                if self.found.limited { "+" } else { "" }
            )
        };
        div()
            .key_context("TerminalSearch")
            .debug_selector(|| "terminal-search".into())
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .p_1()
            .bg(theme.bg_sunken)
            .text_size(UI_FONT_SMALL)
            .text_color(theme.fg_subtle)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::close))
            .child(ui::text_field(self.query.clone(), focused, &theme))
            .child(status)
            .child(ui::button(
                "terminal-find-prev",
                "↑",
                false,
                &theme,
                cx.listener(|this, _, _, cx| this.move_match(true, cx)),
            ))
            .child(ui::button(
                "terminal-find-next",
                "↓",
                false,
                &theme,
                cx.listener(|this, _, _, cx| this.move_match(false, cx)),
            ))
            .child(ui::button(
                "terminal-find-close",
                "×",
                false,
                &theme,
                cx.listener(|this, _, window, cx| this.close(&Close, window, cx)),
            ))
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_reads_history_and_soft_wraps_and_can_be_cancelled() {
        let (tx, _) = futures::channel::mpsc::unbounded();
        let mut term = Term::new(
            Default::default(),
            &super::super::GridSize {
                columns: 8,
                lines: 3,
            },
            Listener(tx),
        );
        let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
        parser.advance(&mut term, b"wrap-match\r\n");
        for _ in 0..40 {
            parser.advance(&mut term, b"other\r\n");
        }
        let term = Arc::new(FairMutex::new(term));
        let counter = AtomicU64::new(7);
        let found = find(&term, "wrap-match", &counter, 7);
        assert_eq!(found.matches.len(), 1);
        assert!(found.matches[0].start.line.0 < 0);
        assert!(found.matches[0].start.line < found.matches[0].end.line);
        assert!(!found.limited);
        assert!(find(&term, "wrap-match", &counter, 6).matches.is_empty());
        assert!(find(&term, "WRAP-MATCH", &counter, 7).matches.is_empty());
        assert!(find(&term, "", &counter, 7).matches.is_empty());
    }
}
