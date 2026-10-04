use gpui::{
    AnyElement, Context, DismissEvent, Entity, SharedString, Task, Window, div, prelude::*,
};

use crate::{
    editor::Editor,
    picker::{Picker, PickerDelegate},
    theme::{ActiveTheme, UI_FONT_SIZE},
};

/// `ctrl-g`: type `42` or `42:7` to jump to line 42, column 7.
pub struct GoToLine {
    editor: Entity<Editor>,
    line_count: usize,
    target: Option<(usize, usize)>,
}

impl GoToLine {
    pub fn new(editor: Entity<Editor>, cx: &gpui::App) -> Self {
        let line_count = editor.read(cx).line_count(cx);
        Self {
            editor,
            line_count,
            target: None,
        }
    }
}

/// Parses `line` or `line:column` (1-based) into a 0-based row and column.
pub fn parse_target(query: &str) -> Option<(usize, usize)> {
    let mut parts = query.trim().splitn(2, [':', ',']);
    let row: usize = parts.next()?.trim().parse().ok()?;
    let col: usize = match parts.next() {
        Some(c) if !c.trim().is_empty() => c.trim().parse().ok()?,
        _ => 1,
    };
    Some((row.max(1) - 1, col.max(1) - 1))
}

impl PickerDelegate for GoToLine {
    fn placeholder(&self) -> SharedString {
        format!("Line number, 1 to {}", self.line_count).into()
    }

    fn match_count(&self) -> usize {
        usize::from(self.target.is_some())
    }

    fn selected_index(&self) -> usize {
        0
    }

    fn set_selected_index(&mut self, _: usize, _: &mut Context<Picker<Self>>) {}

    fn update_matches(
        &mut self,
        query: String,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.target = parse_target(&query).map(|(r, c)| (r.min(self.line_count - 1), c));
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        if let Some((row, col)) = self.target {
            self.editor.update(cx, |e, cx| e.go_to_point(row, col, cx));
        }
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        _: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let (row, col) = self.target.unwrap_or_default();
        let label = if col > 0 {
            format!("Go to line {}, column {}", row + 1, col + 1)
        } else {
            format!("Go to line {}", row + 1)
        };
        div()
            .text_size(UI_FONT_SIZE)
            .text_color(cx.theme().fg)
            .child(label)
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        "Type a line number, or line:column".into()
    }

    fn width(&self) -> gpui::Pixels {
        gpui::px(420.)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_target;

    #[test]
    fn parses_line_and_column() {
        assert_eq!(parse_target("12"), Some((11, 0)));
        assert_eq!(parse_target(" 12:5 "), Some((11, 4)));
        assert_eq!(parse_target("0"), Some((0, 0)));
        assert_eq!(parse_target("x"), None);
    }
}
