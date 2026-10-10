//! The outline of the file in front: its symbols, in the order they are
//! written, each inside the one that holds it.
//!
//! It is what the Structure panel lists and what the breadcrumbs say of
//! the cursor. It comes from the file's language server where one lists
//! symbols, and from the language's own outline where none does. The
//! workspace finds it again a moment after the file changed, and only
//! while something on screen shows it.

use std::{ops::Range, sync::Arc};

use gpui::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, KeyBinding, SharedString,
    UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};
use lsp::{Encoding, types as lt};
use text::Buffer;

use crate::{
    lsp_store::from_range,
    theme::{ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
};

/// How many symbols of a file are kept: a file has more than is read.
pub const MOST: usize = 5000;

/// One symbol of a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub name: String,
    /// The protocol's number for what it is, where it is known.
    pub kind: Option<i32>,
    /// A word about it: its type as the server says it, or what the
    /// language's outline calls such a thing.
    pub detail: String,
    /// How many symbols it is inside of.
    pub depth: usize,
    /// The bytes of all of it, its body included.
    pub range: Range<usize>,
    /// Where the cursor goes for it: its name.
    pub at: usize,
}

/// The symbols a server lists for a file, as the text is now. A server
/// answers with a tree of them or with a plain list.
pub fn of_document(
    response: lt::DocumentSymbolResponse,
    buffer: &Buffer,
    encoding: Encoding,
) -> Vec<Node> {
    fn number(kind: lt::SymbolKind) -> Option<i32> {
        let number = serde_json::to_value(kind).ok()?.as_i64()?;
        Some(number as i32)
    }
    fn walk(
        mut symbols: Vec<lt::DocumentSymbol>,
        depth: usize,
        buffer: &Buffer,
        encoding: Encoding,
        nodes: &mut Vec<Node>,
    ) {
        // Servers may return siblings in any order; hit testing and the
        // trail of parents both need the order in the file.
        symbols.sort_by_key(|symbol| (symbol.range.start.line, symbol.range.start.character));
        for symbol in symbols {
            if nodes.len() >= MOST {
                return;
            }
            nodes.push(Node {
                name: symbol.name,
                kind: number(symbol.kind),
                detail: symbol.detail.unwrap_or_default(),
                depth,
                range: from_range(buffer, symbol.range, encoding),
                at: from_range(buffer, symbol.selection_range, encoding).start,
            });
            let inside = symbol.children.unwrap_or_default();
            walk(inside, depth + 1, buffer, encoding, nodes);
        }
    }
    let mut nodes = Vec::new();
    match response {
        lt::DocumentSymbolResponse::Nested(symbols) => {
            walk(symbols, 0, buffer, encoding, &mut nodes)
        }
        // A plain list says what each is in only by name: they are shown
        // one under another, none inside another.
        lt::DocumentSymbolResponse::Flat(symbols) => {
            nodes.extend(symbols.into_iter().take(MOST).map(|symbol| {
                let range = from_range(buffer, symbol.location.range, encoding);
                Node {
                    name: symbol.name,
                    kind: number(symbol.kind),
                    detail: symbol.container_name.unwrap_or_default(),
                    depth: 0,
                    at: range.start,
                    range,
                }
            }));
            nodes.sort_by_key(|node| node.range.start);
        }
    }
    nodes
}

/// The symbols of a file by its language's outline. An outline says the
/// line each begins at and no more: each reaches to where the next
/// begins, and none is inside another.
pub fn of_outline(symbols: Vec<syntax::Symbol>, buffer: &Buffer) -> Vec<Node> {
    let last = buffer.line_count().saturating_sub(1);
    // An outline counts lines from one.
    let start =
        |symbol: &syntax::Symbol| buffer.line_start(symbol.line.saturating_sub(1).min(last));
    let starts: Vec<usize> = symbols.iter().map(start).collect();
    symbols
        .into_iter()
        .enumerate()
        .map(|(nth, symbol)| Node {
            kind: match symbol.kind.as_ref() {
                "fn" | "def" | "function" | "func" => Some(12),
                "type" | "class" | "struct" | "enum" | "trait" | "interface" => Some(5),
                "const" => Some(14),
                _ => None,
            },
            detail: symbol.kind.into_owned(),
            name: symbol.name,
            depth: 0,
            range: starts[nth]..starts.get(nth + 1).copied().unwrap_or(buffer.len()),
            at: starts[nth],
        })
        .collect()
}

/// The symbol a place in the text is in: the innermost one.
pub fn current(nodes: &[Node], offset: usize) -> Option<usize> {
    // They are in the order they are written, so one inside another
    // comes after it: the last that holds the place is the innermost.
    let before = nodes.partition_point(|node| node.range.start <= offset);
    let holds = |node: &Node| node.range.contains(&offset) || node.range.end == offset;
    nodes[..before].iter().rposition(holds)
}

/// A symbol and the ones it is inside of, the outermost first.
pub fn trail(nodes: &[Node], mut at: usize) -> Vec<usize> {
    let mut trail = vec![at];
    while nodes[at].depth > 0 {
        let depth = nodes[at].depth;
        match nodes[..at].iter().rposition(|node| node.depth < depth) {
            Some(outer) => at = outer,
            None => break,
        }
        trail.push(at);
    }
    trail.reverse();
    trail
}

/// A letter for what a symbol is, in the color of such a thing.
pub fn badge(kind: Option<i32>, theme: &crate::theme::Theme) -> (&'static str, gpui::Hsla) {
    match kind {
        Some(6 | 9 | 12) => ("ƒ", theme.syntax.function),
        Some(7 | 8 | 13 | 20) => ("v", theme.syntax.variable),
        Some(5 | 10 | 11 | 23 | 26) => ("T", theme.syntax.r#type),
        Some(1..=4) => ("m", theme.syntax.r#type),
        Some(14 | 22) => ("c", theme.syntax.number),
        _ => ("·", theme.fg_subtle),
    }
}

actions!(structure, [Next, Previous, Open]);

pub fn bind_keys(cx: &mut App) {
    let context = Some("Structure");
    cx.bind_keys([
        KeyBinding::new("down", Next, context),
        KeyBinding::new("up", Previous, context),
        KeyBinding::new("enter", Open, context),
    ]);
}

pub enum StructureEvent {
    /// Go to this place in the file in front.
    Jump(usize),
}

/// The Structure panel: the outline of the file in front as a list, each
/// symbol as far in as it is inside others.
pub struct StructurePanel {
    nodes: Arc<Vec<Node>>,
    /// The symbol the cursor is in.
    current: Option<usize>,
    selected: usize,
    /// What to say while there is nothing to list.
    empty: SharedString,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
}

impl EventEmitter<StructureEvent> for StructurePanel {}

impl Focusable for StructurePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl StructurePanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            nodes: Arc::default(),
            current: None,
            selected: 0,
            empty: "The structure of the file in front is shown here".into(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
        }
    }

    /// The outline of the file in front, as it is now. `file` is whether
    /// there is a file in front at all.
    pub fn set(&mut self, nodes: Arc<Vec<Node>>, file: bool, cx: &mut Context<Self>) {
        self.empty = match file {
            true => "No symbols in this file".into(),
            false => "The structure of the file in front is shown here".into(),
        };
        self.selected = self.selected.min(nodes.len().saturating_sub(1));
        self.nodes = nodes;
        cx.notify();
    }

    /// The symbol the cursor is in now. The list follows it while the
    /// keys are not in the list itself.
    pub fn set_current(&mut self, current: Option<usize>, window: &Window, cx: &mut Context<Self>) {
        if self.current == current {
            return;
        }
        self.current = current;
        if let Some(current) = current.filter(|_| !self.focus.contains_focused(window, cx)) {
            self.selected = current;
            self.scroll
                .scroll_to_item(current, gpui::ScrollStrategy::Center);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn shown(&self) -> Vec<String> {
        let line = |(at, node): (usize, &Node)| {
            let mark = if self.current == Some(at) { "> " } else { "" };
            let detail = match node.detail.is_empty() {
                true => String::new(),
                false => format!(" ({})", node.detail),
            };
            format!("{mark}{}{}{detail}", "  ".repeat(node.depth), node.name)
        };
        self.nodes.iter().enumerate().map(line).collect()
    }

    fn select(&mut self, at: usize, cx: &mut Context<Self>) {
        if at < self.nodes.len() {
            self.selected = at;
            self.scroll.scroll_to_item(at, gpui::ScrollStrategy::Top);
            cx.notify();
        }
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected + 1, cx);
    }

    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected.saturating_sub(1), cx);
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(node) = self.nodes.get(self.selected) {
            cx.emit(StructureEvent::Jump(node.at));
        }
    }

    fn rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let focused = self.focus.contains_focused(window, cx);
        range
            .map(|at| {
                let node = &self.nodes[at];
                let (letter, color) = badge(node.kind, &theme);
                let here = self.current == Some(at);
                div()
                    .id(("structure-row", at))
                    .debug_selector(move || format!("structure-{at}"))
                    .w_full()
                    .h(crate::theme::row(px(24.), cx))
                    .pl(px(8. + node.depth as f32 * 14.))
                    .pr_2()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(UI_FONT_SIZE)
                    .text_color(if here { theme.fg } else { theme.fg_muted })
                    .when(self.selected == at, |d| {
                        d.bg(if focused {
                            theme.accent_soft
                        } else {
                            theme.bg_elev
                        })
                    })
                    .hover(|d| d.bg(theme.bg_elev))
                    .child(
                        div()
                            .flex_none()
                            .w(px(12.))
                            .font_family(crate::theme::CODE_FONT)
                            .text_color(color)
                            .child(letter),
                    )
                    .child(div().flex_none().child(node.name.clone()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(UI_FONT_SMALL)
                            .text_color(theme.fg_subtle)
                            .child(node.detail.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = at;
                        if let Some(node) = this.nodes.get(at) {
                            cx.emit(StructureEvent::Jump(node.at));
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect()
    }
}

impl Render for StructurePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context("Structure")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open))
            .when(self.nodes.is_empty(), |d| {
                d.child(
                    div()
                        .p_3()
                        .text_size(UI_FONT_SIZE)
                        .text_color(theme.fg_muted)
                        .child(self.empty.clone()),
                )
            })
            .child(
                uniform_list(
                    "structure",
                    self.nodes.len(),
                    cx.processor(|this, range: Range<usize>, window, cx| {
                        this.rows(range, window, cx)
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, depth: usize, range: Range<usize>) -> Node {
        Node {
            name: name.into(),
            kind: None,
            detail: String::new(),
            depth,
            at: range.start,
            range,
        }
    }

    #[test]
    fn the_symbol_a_place_is_in_is_the_innermost() {
        // a(0..50) { b(10..30) { c(12..20) } d(32..40) } e(60..70)
        let nodes = [
            node("a", 0, 0..50),
            node("b", 1, 10..30),
            node("c", 2, 12..20),
            node("d", 1, 32..40),
            node("e", 0, 60..70),
        ];
        let named = |offset: usize| current(&nodes, offset).map(|at| nodes[at].name.as_str());
        assert_eq!(named(5), Some("a"));
        assert_eq!(named(15), Some("c"));
        assert_eq!(named(25), Some("b"));
        assert_eq!(named(35), Some("d"));
        assert_eq!(named(45), Some("a"));
        // Between two symbols is in neither; the end of one is still it.
        assert_eq!(named(55), None);
        assert_eq!(named(70), Some("e"));
        assert_eq!(current(&[], 3), None);
        // The ones a symbol is inside of, from the outermost.
        let names = |at: usize| -> Vec<&str> {
            let trail = trail(&nodes, at);
            trail
                .into_iter()
                .map(|at| nodes[at].name.as_str())
                .collect()
        };
        assert_eq!(names(2), ["a", "b", "c"]);
        assert_eq!(names(3), ["a", "d"]);
        assert_eq!(names(4), ["e"]);
    }

    #[test]
    fn an_outline_s_symbols_reach_to_the_next() {
        let buffer = Buffer::new("fn a() {}\n\nstruct B;\nconst C: u8 = 1;\n");
        let symbol = |name: &str, kind: &'static str, line: usize| syntax::Symbol {
            name: name.into(),
            kind: kind.into(),
            line,
        };
        let nodes = of_outline(
            vec![
                symbol("a", "fn", 1),
                symbol("B", "type", 3),
                symbol("C", "const", 4),
            ],
            &buffer,
        );
        let seen: Vec<_> = nodes
            .iter()
            .map(|node| (node.name.as_str(), node.kind, node.range.clone(), node.at))
            .collect();
        assert_eq!(
            seen,
            [
                ("a", Some(12), 0..11, 0),
                ("B", Some(5), 11..21, 11),
                ("C", Some(14), 21..buffer.len(), 21),
            ]
        );
        assert_eq!(
            current(&nodes, 12).map(|at| nodes[at].name.as_str()),
            Some("B")
        );
    }

    #[test]
    fn a_server_s_tree_of_symbols_is_a_list_with_depths() {
        let buffer = Buffer::new("impl A {\n    fn b() {}\n}\nfn c() {}\n");
        let range = |a: (u32, u32), b: (u32, u32)| {
            lt::Range::new(lt::Position::new(a.0, a.1), lt::Position::new(b.0, b.1))
        };
        #[allow(deprecated)]
        let symbol =
            |name: &str, kind, range: lt::Range, name_at: lt::Range, children| lt::DocumentSymbol {
                name: name.into(),
                detail: (name == "b").then(|| "fn()".to_string()),
                kind,
                tags: None,
                deprecated: None,
                range,
                selection_range: name_at,
                children,
            };
        let b = symbol(
            "b",
            lt::SymbolKind::METHOD,
            range((1, 4), (1, 13)),
            range((1, 7), (1, 8)),
            None,
        );
        let a = symbol(
            "A",
            lt::SymbolKind::CLASS,
            range((0, 0), (2, 1)),
            range((0, 5), (0, 6)),
            Some(vec![b]),
        );
        let c = symbol(
            "c",
            lt::SymbolKind::FUNCTION,
            range((3, 0), (3, 9)),
            range((3, 3), (3, 4)),
            None,
        );
        let nodes = of_document(
            lt::DocumentSymbolResponse::Nested(vec![c, a]),
            &buffer,
            Encoding::Utf8,
        );
        let seen: Vec<_> = nodes
            .iter()
            .map(|node| {
                (
                    node.name.as_str(),
                    node.depth,
                    node.range.clone(),
                    node.at,
                    node.detail.as_str(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("A", 0, 0..24, 5, ""),
                ("b", 1, 13..22, 16, "fn()"),
                ("c", 0, 25..34, 28, ""),
            ]
        );
    }
}
