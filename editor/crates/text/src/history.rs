use std::time::{Duration, Instant};

use crate::Selection;

/// One replaced span, recorded with the offset it had when it was applied.
#[derive(Clone, Debug)]
pub(crate) struct Change {
    pub start: usize,
    pub old_text: String,
    pub new_text: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Transaction {
    /// In application order.
    pub changes: Vec<Change>,
    pub before: Vec<Selection>,
    pub after: Vec<Selection>,
    last_edit_at: Instant,
}

/// Undo stack with time-based grouping, so a burst of typing undoes as one step.
#[derive(Default)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    sealed: bool,
}

impl History {
    pub const GROUP_INTERVAL: Duration = Duration::from_millis(300);

    pub(crate) fn push(&mut self, changes: Vec<Change>, before: &[Selection], now: Instant) {
        self.redo.clear();
        if !self.sealed
            && let Some(last) = self.undo.last_mut()
            && now.saturating_duration_since(last.last_edit_at) < Self::GROUP_INTERVAL
        {
            last.changes.extend(changes);
            last.last_edit_at = now;
            return;
        }
        self.sealed = false;
        self.undo.push(Transaction {
            changes,
            before: before.to_vec(),
            after: Vec::new(),
            last_edit_at: now,
        });
    }

    pub(crate) fn set_selections_after(&mut self, selections: &[Selection]) {
        if let Some(last) = self.undo.last_mut() {
            last.after = selections.to_vec();
        }
    }

    pub(crate) fn seal(&mut self) {
        self.sealed = true;
    }

    pub(crate) fn pop_undo(&mut self) -> Option<Transaction> {
        self.sealed = true;
        self.undo.pop()
    }

    pub(crate) fn push_redo(&mut self, tx: Transaction) {
        self.redo.push(tx);
    }

    pub(crate) fn pop_redo(&mut self) -> Option<Transaction> {
        self.redo.pop()
    }

    pub(crate) fn push_undo_after_redo(&mut self, tx: Transaction) {
        self.sealed = true;
        self.undo.push(tx);
    }
}
