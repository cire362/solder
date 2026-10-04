//! Git actions in the editor: stage selected lines, revert a hunk, step
//! through hunks, and resolve merge conflicts.

use std::ops::Range;

use gpui::{Context, Window};
use text::diff::{Hunk, lines, replace_rows};

use crate::{
    editor::{
        AcceptBoth, AcceptOurs, AcceptTheirs, Editor, EditorEvent, NextConflict, NextHunk,
        PrevHunk, RevertHunk, StageLines,
    },
    git::hunk_touched,
};

impl Editor {
    /// Rows covered by all selections, as one range.
    fn selected_rows(&self, cx: &gpui::App) -> Range<usize> {
        let buffer = self.buf(cx);
        let start = self
            .selections
            .iter()
            .map(|s| buffer.offset_to_point(s.range().start).row)
            .min()
            .unwrap_or(0);
        let end = self
            .selections
            .iter()
            .map(|s| {
                let p = buffer.offset_to_point(s.range().end);
                // A selection ending at column 0 does not include that row.
                if p.column == 0 && !s.is_empty() {
                    p.row
                } else {
                    p.row + 1
                }
            })
            .max()
            .unwrap_or(start + 1);
        start..end.max(start + 1)
    }

    pub(crate) fn stage_lines(&mut self, _: &StageLines, _: &mut Window, cx: &mut Context<Self>) {
        if self.doc(cx).path().is_none() {
            return;
        }
        let rows = self.selected_rows(cx);
        cx.emit(EditorEvent::StageRows { rows });
    }

    /// Replaces the hunks under the selection with their staged text.
    pub(crate) fn revert_hunk(&mut self, _: &RevertHunk, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.selected_rows(cx);
        let doc = self.doc(cx);
        let Some(base) = doc.diff_base().cloned() else {
            return;
        };
        let current = doc.text().rope().to_string();
        let base_lines = lines(&base);
        let edits: Vec<(Range<usize>, String)> = doc
            .hunks()
            .iter()
            .filter(|h| hunk_touched(h, &rows))
            .map(|h| replace_rows(&current, h.new.clone(), &base_lines[h.old.clone()]))
            .collect();
        if edits.is_empty() {
            return;
        }
        self.replace_ranges(edits, cx);
    }

    fn jump_to_hunk(&mut self, forward: bool, cx: &mut Context<Self>) {
        let row = self.buf(cx).offset_to_point(self.newest_range().start).row;
        let hunks = self.doc(cx).hunks().clone();
        let target: Option<&Hunk> = if forward {
            hunks.iter().find(|h| h.new.start > row).or(hunks.first())
        } else {
            hunks
                .iter()
                .rev()
                .find(|h| h.new.start < row)
                .or(hunks.last())
        };
        if let Some(h) = target {
            let start = h.new.start;
            self.go_to_point(start, 0, cx);
        }
    }

    pub(crate) fn next_hunk(&mut self, _: &NextHunk, _: &mut Window, cx: &mut Context<Self>) {
        self.jump_to_hunk(true, cx);
    }

    pub(crate) fn prev_hunk(&mut self, _: &PrevHunk, _: &mut Window, cx: &mut Context<Self>) {
        self.jump_to_hunk(false, cx);
    }

    /// Keeps one or both sides of the conflict under the cursor.
    fn resolve_conflict(&mut self, ours: bool, theirs: bool, cx: &mut Context<Self>) {
        let row = self.buf(cx).offset_to_point(self.newest_range().start).row;
        let doc = self.doc(cx);
        let Some(conflict) = doc
            .conflicts()
            .iter()
            .find(|c| c.start <= row && row <= c.end)
            .cloned()
        else {
            return;
        };
        let current = doc.text().rope().to_string();
        let all = lines(&current);
        let mut keep: Vec<&str> = Vec::new();
        if ours {
            keep.extend_from_slice(&all[conflict.ours()]);
        }
        if theirs {
            keep.extend_from_slice(&all[conflict.theirs()]);
        }
        let edit = replace_rows(&current, conflict.start..conflict.end + 1, &keep);
        let start = edit.0.start;
        self.replace_ranges(vec![edit], cx);
        self.select_range(start..start, cx);
    }

    pub(crate) fn accept_ours(&mut self, _: &AcceptOurs, _: &mut Window, cx: &mut Context<Self>) {
        self.resolve_conflict(true, false, cx);
    }

    pub(crate) fn accept_theirs(
        &mut self,
        _: &AcceptTheirs,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resolve_conflict(false, true, cx);
    }

    pub(crate) fn accept_both(&mut self, _: &AcceptBoth, _: &mut Window, cx: &mut Context<Self>) {
        self.resolve_conflict(true, true, cx);
    }

    pub(crate) fn next_conflict(
        &mut self,
        _: &NextConflict,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = self.buf(cx).offset_to_point(self.newest_range().start).row;
        let conflicts = self.doc(cx).conflicts().clone();
        if let Some(c) = conflicts
            .iter()
            .find(|c| c.start > row)
            .or(conflicts.first())
        {
            let start = c.start;
            self.go_to_point(start, 0, cx);
        }
    }
}
