//! The editor's custom element. Only rows inside the viewport are shaped and
//! painted, so frame cost depends on window height, not file length.

use std::{ops::Range, sync::Arc, time::Instant};

use gpui::{
    App, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, Point, ShapedLine, SharedString,
    Style, TextRun, UnderlineStyle, Window, fill, point, px, relative, size,
};
use syntax::HighlightKind;
use text::Buffer;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    document::{Diagnostic, Document, Severity},
    editor::{Editor, EditorMode, HighlightCache},
    perf::Perf,
    settings::Settings,
    theme::{ActiveTheme, Theme},
};

/// Longest slice of a single line that gets shaped. Minified bundles can have
/// megabyte-long lines; nobody reads past this without wrapping.
const MAX_SHAPED_BYTES: usize = 16 * 1024;
const GUTTER_PADDING: Pixels = px(16.);

pub struct EditorElement {
    editor: Entity<Editor>,
}

impl EditorElement {
    pub fn new(editor: Entity<Editor>) -> Self {
        Self { editor }
    }
}

/// What stands between two lenses of one line.
const LENS_GAP: &str = "  |  ";

/// The lines of the file that have a row of lenses above them, in order
/// and each once. With any, a line is no longer drawn in the row of its
/// own number: everything that turns a line into a height asks here.
#[derive(Clone, Default)]
pub struct Rows(Arc<Vec<usize>>);

impl Rows {
    /// The lines of these lenses, in a text.
    pub(crate) fn of_lenses(lenses: &[crate::document::Lens], buffer: &Buffer) -> Self {
        let mut rows: Vec<usize> = lenses
            .iter()
            .map(|lens| buffer.offset_to_point(lens.offset.min(buffer.len())).row)
            .collect();
        rows.sort_unstable();
        rows.dedup();
        Self(Arc::new(rows))
    }

    /// How many rows there are above the lines.
    fn added(&self) -> usize {
        self.0.len()
    }

    /// The row on screen a line of the file is drawn in.
    fn shown(&self, row: usize) -> usize {
        row + self.0.partition_point(|lensed| *lensed <= row)
    }

    /// The line a row on screen belongs to, and whether the row is the
    /// lenses above that line and not the line itself.
    fn line(&self, shown: usize) -> (usize, bool) {
        // The lenses of the nth such line are in row `line + n`.
        let (mut low, mut high) = (0, self.0.len());
        while low < high {
            let middle = (low + high) / 2;
            if self.0[middle] + middle <= shown {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        match low.checked_sub(1) {
            Some(last) if self.0[last] + last == shown => (self.0[last], true),
            _ => (shown - low, false),
        }
    }
}

/// The lenses of one line as they are drawn above it: their words, how
/// far in they start (the line's own indent), and which bytes of the
/// words are which of the document's lenses.
pub struct LensRow {
    row: usize,
    shaped: ShapedLine,
    indent: Pixels,
    parts: Vec<(Range<usize>, usize)>,
}

/// A shaped row plus the bookkeeping to map buffer columns to shaped columns.
pub struct DisplayLine {
    pub shaped: ShapedLine,
    /// `(byte column of a tab, extra bytes it expanded into)`, in order.
    tabs: Vec<(usize, usize)>,
    /// `(byte column a hint is drawn before, its bytes)`, in order: text
    /// of a language server's that is in the row and not in the file.
    inlays: Vec<(usize, usize)>,
    /// Byte length of the row that was shaped (rows can be truncated).
    len: usize,
}

impl DisplayLine {
    /// Where a column of the file is in what was drawn. A hint at the
    /// column itself comes after it: the cursor stands before the hint.
    fn expand(&self, col: usize) -> usize {
        let col = col.min(self.len);
        let before = |list: &[(usize, usize)]| -> usize {
            list.iter()
                .take_while(|(at, _)| *at < col)
                .map(|(_, extra)| extra)
                .sum()
        };
        col + before(&self.tabs) + before(&self.inlays)
    }

    /// The same, past the hints drawn at the column.
    fn expand_after(&self, col: usize) -> usize {
        let col = col.min(self.len);
        let here = self.inlays.iter().filter(|(at, _)| *at == col);
        self.expand(col) + here.map(|(_, bytes)| bytes).sum::<usize>()
    }

    fn collapse(&self, expanded: usize) -> usize {
        let mut shift = 0;
        let (mut tabs, mut inlays) = (self.tabs.iter().peekable(), self.inlays.iter().peekable());
        loop {
            // What was put into the row, in the order it stands there: a
            // hint at a tab's column is drawn before the tab.
            let hint_first = match (inlays.peek(), tabs.peek()) {
                (Some((hint, _)), Some((tab, _))) => hint <= tab,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if hint_first {
                let Some((at, bytes)) = inlays.next() else {
                    break;
                };
                let start = at + shift;
                if expanded <= start {
                    break;
                }
                // Inside a hint is the column it is drawn at.
                if expanded < start + bytes {
                    return *at;
                }
                shift += bytes;
                continue;
            }
            let Some((at, extra)) = tabs.next() else {
                break;
            };
            let start = at + shift;
            if expanded <= start {
                break;
            }
            if expanded <= start + extra {
                // Inside the spaces a tab became: snap to the nearer side.
                return if expanded - start > extra.div_ceil(2) {
                    at + 1
                } else {
                    *at
                };
            }
            shift += extra;
        }
        (expanded - shift).min(self.len)
    }

    pub fn x_for(&self, col: usize) -> Pixels {
        self.shaped.x_for_index(self.expand(col))
    }
}

/// What the last frame drew. Hit testing and IME positioning read this.
pub struct LayoutSnapshot {
    pub bounds: Bounds<Pixels>,
    pub text_left: Pixels,
    pub line_height: Pixels,
    pub em_width: Pixels,
    pub first_row: usize,
    pub lines: Vec<DisplayLine>,
    /// Which lines have a row of lenses above them, and those rows.
    pub rows: Rows,
    pub lens_rows: Vec<LensRow>,
}

impl LayoutSnapshot {
    /// The row on screen under a height, counted from the first.
    fn shown_at(&self, scroll: Point<Pixels>, y: Pixels) -> usize {
        let row = ((y - self.bounds.top() + scroll.y) / self.line_height).floor();
        row.max(0.) as usize
    }

    /// Where the top of a line of the file is, were nothing scrolled.
    pub(crate) fn top_of(&self, row: usize) -> Pixels {
        self.bounds.top() + self.line_height * self.rows.shown(row) as f32
    }

    pub(crate) fn is_lens_row(&self, scroll: Point<Pixels>, y: Pixels) -> bool {
        self.rows.line(self.shown_at(scroll, y)).1
    }

    pub(crate) fn row_at(&self, buffer: &Buffer, scroll: Point<Pixels>, y: Pixels) -> usize {
        let (row, _) = self.rows.line(self.shown_at(scroll, y));
        row.min(buffer.line_count() - 1)
    }

    /// The lens whose words are under the pointer, as its place among the
    /// document's.
    pub fn lens_at(&self, scroll: Point<Pixels>, position: Point<Pixels>) -> Option<usize> {
        if !self.bounds.contains(&position) || position.x < self.text_left {
            return None;
        }
        let (row, lenses) = self.rows.line(self.shown_at(scroll, position.y));
        let above = self.lens_rows.iter().find(|above| above.row == row)?;
        let x = position.x - self.text_left + scroll.x - above.indent;
        let at = above.shaped.index_for_x(x).filter(|_| lenses)?;
        let found = above.parts.iter().find(|(words, _)| words.contains(&at));
        found.map(|(_, lens)| *lens)
    }

    /// The middle of the words of a lens above a line, for a test to
    /// click: the `nth` of that line's.
    #[cfg(test)]
    pub(crate) fn lens_middle(&self, row: usize, nth: usize) -> Option<Point<Pixels>> {
        let above = self.lens_rows.iter().find(|above| above.row == row)?;
        let (words, _) = above.parts.get(nth)?;
        let (start, end) = (
            above.shaped.x_for_index(words.start),
            above.shaped.x_for_index(words.end),
        );
        Some(gpui::point(
            self.text_left + above.indent + (start + end) / 2.,
            self.top_of(row) - self.line_height / 2.,
        ))
    }

    pub fn offset_for_position(
        &self,
        buffer: &Buffer,
        scroll: Point<Pixels>,
        position: Point<Pixels>,
    ) -> usize {
        let row = self.row_at(buffer, scroll, position.y);
        let x = position.x - self.text_left + scroll.x;
        let col = match row
            .checked_sub(self.first_row)
            .and_then(|i| self.lines.get(i))
        {
            Some(line) => line.collapse(line.shaped.closest_index_for_x(x)),
            None => {
                let cells = (x / self.em_width).round().max(0.) as usize;
                buffer.column_for_display(row, cells)
            }
        };
        buffer.point_to_offset(text::Point::new(row, col))
    }

    /// Offset under the pointer, only when it is over actual text (not the
    /// gutter, not past the end of a line). Used for hover.
    pub fn text_offset_at(
        &self,
        buffer: &Buffer,
        scroll: Point<Pixels>,
        position: Point<Pixels>,
    ) -> Option<usize> {
        if !self.bounds.contains(&position) || position.x < self.text_left {
            return None;
        }
        // The row of a line's lenses is no text of the file.
        let (row, lenses) = self.rows.line(self.shown_at(scroll, position.y));
        if row >= buffer.line_count() || lenses {
            return None;
        }
        let line = self.lines.get(row.checked_sub(self.first_row)?)?;
        let x = position.x - self.text_left + scroll.x;
        if x > line.shaped.width {
            return None;
        }
        let col = line.collapse(line.shaped.closest_index_for_x(x));
        Some(buffer.point_to_offset(text::Point::new(row, col)))
    }

    pub fn bounds_for_offset(
        &self,
        buffer: &Buffer,
        scroll: Point<Pixels>,
        offset: usize,
    ) -> Option<Bounds<Pixels>> {
        let p = buffer.offset_to_point(offset);
        let line = self.lines.get(p.row.checked_sub(self.first_row)?)?;
        let x = self.text_left + line.x_for(p.column) - scroll.x;
        let y = self.top_of(p.row) - scroll.y;
        Some(Bounds::new(
            point(x, y),
            size(self.em_width, self.line_height),
        ))
    }
}

pub struct PrepaintState {
    layout: Option<LayoutSnapshot>,
    started: Instant,
    scroll: Point<Pixels>,
    text_bounds: Bounds<Pixels>,
    background: Vec<PaintQuad>,
    /// Search matches and the matching bracket, under the selection.
    highlights: Vec<PaintQuad>,
    selections: Vec<PaintQuad>,
    placeholder: Option<ShapedLine>,
    cursors: Vec<PaintQuad>,
    /// A suggestion at the cursor: its lines, and backgrounds that hide the
    /// rows its later lines are drawn over.
    ghost: Vec<(ShapedLine, Point<Pixels>)>,
    ghost_background: Vec<PaintQuad>,
    gutter: Vec<(ShapedLine, Point<Pixels>)>,
    /// Where the row of the cursor is, when an editor as tall as its text
    /// wants it in view.
    reveal: Option<(Pixels, Pixels)>,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let single_line = self.editor.read(cx).mode == EditorMode::SingleLine;
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let editor = self.editor.read(cx);
        style.size.height = if single_line {
            Settings::get(cx).line_height().into()
        } else if editor.fit.is_some() {
            // As tall as its text.
            let lines = editor.doc(cx).text().line_count();
            (Settings::get(cx).line_height() * lines as f32).into()
        } else {
            relative(1.).into()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        let started = Instant::now();
        let theme = cx.theme().clone();
        let settings = Settings::get(cx).clone();
        let focused = self.editor.read(cx).focus_handle_ref().is_focused(window);
        let mut state = self.editor.update(cx, |editor, cx| {
            layout(editor, bounds, focused, &theme, &settings, window, cx)
        });
        state.started = started;
        // What the editor is in is asked to show the cursor once this
        // frame is done with: it may be in the middle of its own.
        if let Some((top, bottom)) = state.reveal.take()
            && let Some(reveal) = self.editor.read(cx).fit.clone()
        {
            cx.defer(move |cx| reveal(top, bottom, cx));
        }
        state
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.editor.read(cx).focus_handle_ref().clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );

        // Rows scrolled partly out of view must not spill over what is above
        // or below the editor (the tab bar): clip to its bounds.
        let mask = Some(ContentMask { bounds });
        window.with_content_mask(mask.clone(), |window| {
            for quad in state.background.drain(..) {
                window.paint_quad(quad);
            }
        });
        let layout = state
            .layout
            .take()
            .expect("prepaint always produces a layout");
        let scroll = state.scroll;
        if let Some(placeholder) = state.placeholder.take() {
            let origin = point(state.text_bounds.left(), bounds.top());
            placeholder
                .paint(origin, layout.line_height, window, cx)
                .ok();
        }
        window.with_content_mask(
            Some(ContentMask {
                bounds: state.text_bounds,
            }),
            |window| {
                for quad in state.highlights.drain(..) {
                    window.paint_quad(quad);
                }
                for quad in state.selections.drain(..) {
                    window.paint_quad(quad);
                }
                for (i, line) in layout.lines.iter().enumerate() {
                    let row = layout.first_row + i;
                    let origin = point(layout.text_left - scroll.x, layout.top_of(row) - scroll.y);
                    line.shaped
                        .paint(origin, layout.line_height, window, cx)
                        .ok();
                }
                for above in &layout.lens_rows {
                    let origin = point(
                        layout.text_left + above.indent - scroll.x,
                        layout.top_of(above.row) - layout.line_height - scroll.y,
                    );
                    above
                        .shaped
                        .paint(origin, layout.line_height, window, cx)
                        .ok();
                }
                for quad in state.ghost_background.drain(..) {
                    window.paint_quad(quad);
                }
                for (line, origin) in state.ghost.drain(..) {
                    line.paint(origin, layout.line_height, window, cx).ok();
                }
                for quad in state.cursors.drain(..) {
                    window.paint_quad(quad);
                }
            },
        );
        window.with_content_mask(mask, |window| {
            for (number, origin) in state.gutter.drain(..) {
                number.paint(origin, layout.line_height, window, cx).ok();
            }
        });

        self.editor
            .update(cx, |editor, _| editor.layout = Some(layout));
        Perf::frame_painted(cx, state.started.elapsed());
    }
}

fn layout(
    editor: &mut Editor,
    bounds: Bounds<Pixels>,
    focused: bool,
    theme: &Theme,
    settings: &Settings,
    window: &mut Window,
    cx: &App,
) -> PrepaintState {
    let document = editor.document.clone();
    let doc = document.read(cx);
    let text_system = window.text_system().clone();
    let code_font = settings.buffer_font();
    let font_size = settings.buffer_font_size();
    let font_id = text_system.resolve_font(&code_font);
    let em = text_system
        .advance(font_id, font_size, 'm')
        .map_or(px(8.), |s| s.width);
    let lh = settings.line_height();

    let single_line = editor.mode == EditorMode::SingleLine;
    let buffer = doc.text();
    let line_count = buffer.line_count();
    let digits = line_count.to_string().len().max(3);
    let gutter_width = if single_line {
        px(0.)
    } else {
        em * digits as f32 + GUTTER_PADDING * 2.
    };
    let text_left = bounds.left() + gutter_width;
    let text_bounds = Bounds::new(
        point(text_left, bounds.top()),
        size(bounds.size.width - gutter_width, bounds.size.height),
    );

    // Vertical: follow the newest cursor with a few rows of margin, then clamp.
    let height = bounds.size.height;
    let mut scroll = editor.scroll;
    let newest_head = editor.selections[editor.newest].head;
    let cursor_row = buffer.offset_to_point(newest_head).row;
    // The lines that have their lenses above them. An editor as tall as
    // its text has no server, and a field has one line.
    let rows = match single_line || editor.fit.is_some() || doc.lenses().is_empty() {
        true => Rows::default(),
        false => Rows::of_lenses(doc.lenses(), buffer),
    };
    if editor.autoscroll {
        let margin = if single_line {
            px(0.)
        } else {
            (lh * 3.).min(height / 3.)
        };
        let top = lh * rows.shown(cursor_row) as f32;
        if top - margin < scroll.y {
            scroll.y = top - margin;
        } else if top + lh + margin > scroll.y + height {
            scroll.y = top + lh + margin - height;
        }
    }
    let max_y = lh * (line_count + rows.added()).saturating_sub(1) as f32;
    scroll.y = clamp(scroll.y, px(0.), max_y);

    let line_at = |y: Pixels| rows.line((y / lh).floor().max(0.) as usize).0;
    let mut first_row = line_at(scroll.y).min(line_count - 1);
    let mut end_row = (line_at(scroll.y + height) + 2).min(line_count);
    let mut reveal = None;
    if editor.fit.is_some() {
        // As tall as its text, it does not scroll: the rows to draw are
        // the ones its place in the window leaves to be seen.
        scroll.y = px(0.);
        let seen = window.content_mask().bounds;
        let from = (seen.top() - bounds.top()).max(px(0.));
        let to = (seen.bottom() - bounds.top()).max(px(0.));
        first_row = ((from / lh).floor() as usize).min(line_count - 1);
        end_row = ((to / lh).ceil() as usize + 1).clamp(first_row + 1, line_count);
        if editor.autoscroll {
            let top = bounds.top() + lh * cursor_row as f32;
            reveal = Some((top, top + lh));
        }
    }
    let visible = buffer.line_start(first_row)..if end_row < line_count {
        buffer.line_start(end_row)
    } else {
        buffer.len()
    };

    let spans = highlights(editor, doc, visible.clone());

    // Diagnostics on screen, most severe first so they win overlaps.
    let mut visible_diagnostics: Vec<&Diagnostic> = doc
        .diagnostics()
        .iter()
        .filter(|d| d.range.start <= visible.end && d.range.end >= visible.start)
        .collect();
    visible_diagnostics.sort_by_key(|d| d.severity);
    let mut underlines: Vec<(Range<usize>, UnderlineStyle)> = Vec::new();
    if let Some(m) = &editor.marked_range {
        underlines.push((
            m.clone(),
            UnderlineStyle {
                color: None,
                thickness: px(1.),
                wavy: false,
            },
        ));
    }
    for d in &visible_diagnostics {
        let color = severity_color(d.severity, theme);
        // Zero-width diagnostics still get one character of squiggle.
        let end = if d.range.is_empty() {
            buffer.next_grapheme(d.range.start)
        } else {
            d.range.end
        };
        underlines.push((
            d.range.start..end,
            UnderlineStyle {
                color: Some(color),
                thickness: px(1.),
                wavy: true,
            },
        ));
    }

    let mut lines = Vec::with_capacity(end_row - first_row);
    let mut span_ix = spans.partition_point(|(r, _)| r.end <= visible.start);
    // What a language server puts into the rows on screen.
    let inlays = doc.inlays();
    let mut inlay_ix = inlays.partition_point(|inlay| inlay.offset < visible.start);
    for row in first_row..end_row {
        let line_start = buffer.line_start(row);
        let full = buffer.line_str(row);
        let mut len = full.len().min(MAX_SHAPED_BYTES);
        while !full.is_char_boundary(len) {
            len -= 1;
        }
        // A masked field (an API key) draws one star per byte, so offsets
        // and the cursor still line up.
        let stars: String;
        let text = if editor.masked {
            stars = "*".repeat(len);
            &stars
        } else {
            &full[..len]
        };
        let (segments, next_ix) = color_segments(text, line_start, &spans, span_ix, theme);
        span_ix = next_ix;
        let mut hints: Vec<(usize, &str)> = Vec::new();
        while let Some(inlay) = inlays
            .get(inlay_ix)
            .filter(|i| i.offset <= line_start + full.len())
        {
            // Not in the part of a very long row that is left undrawn.
            if inlay.offset <= line_start + len && !editor.masked {
                hints.push((inlay.offset.saturating_sub(line_start), &inlay.text));
            }
            inlay_ix += 1;
        }
        lines.push(shape_row(
            text,
            line_start,
            segments,
            &underlines,
            (&hints, theme.fg_subtle),
            (&code_font, font_size),
            window,
        ));
    }

    // What a server offers to do with a line, above it: the words of its
    // lenses one after another, starting where the line's own text does.
    let mut lens_rows = Vec::new();
    let lenses = doc.lenses();
    let mut lens_ix = lenses.partition_point(|lens| lens.offset < visible.start);
    while let Some(lens) = lenses.get(lens_ix).filter(|_| rows.added() > 0) {
        let row = buffer.offset_to_point(lens.offset.min(buffer.len())).row;
        if row >= end_row {
            break;
        }
        let mut words = String::new();
        let mut parts = Vec::new();
        while let Some(lens) = lenses
            .get(lens_ix)
            .filter(|lens| buffer.offset_to_point(lens.offset.min(buffer.len())).row == row)
        {
            if !words.is_empty() {
                words.push_str(LENS_GAP);
            }
            let title = lens.title.trim();
            parts.push((words.len()..words.len() + title.len(), lens_ix));
            words.push_str(title);
            lens_ix += 1;
        }
        let Some(line) = row.checked_sub(first_row).and_then(|at| lines.get(at)) else {
            continue;
        };
        let full = buffer.line_str(row);
        let indent = full.len() - full.trim_start().len();
        let run = TextRun {
            len: words.len(),
            font: code_font.clone(),
            color: theme.fg_subtle,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        lens_rows.push(LensRow {
            row,
            shaped: window
                .text_system()
                .shape_line(words.into(), font_size, &[run], None),
            indent: line.x_for(indent),
            parts,
        });
    }

    // Horizontal: keep the newest cursor in view, then clamp to content width.
    let text_width = text_bounds.size.width;
    if editor.autoscroll
        && let Some(line) = lines.get(cursor_row.saturating_sub(first_row))
        && cursor_row >= first_row
    {
        let col = newest_head - buffer.line_start(cursor_row);
        let x = line.x_for(col);
        if x - em * 2. < scroll.x {
            scroll.x = x - em * 2.;
        } else if x + em * 2. > scroll.x + text_width {
            scroll.x = x + em * 2. - text_width;
        }
    }
    let widest = lines
        .iter()
        .map(|l| l.shaped.width)
        .chain(
            lens_rows
                .iter()
                .map(|above| above.indent + above.shaped.width),
        )
        .fold(px(0.), |a, b| if b > a { b } else { a });
    let max_x = widest + em * 2. - text_width;
    scroll.x = clamp(
        scroll.x,
        px(0.),
        if max_x > px(0.) { max_x } else { px(0.) },
    );
    editor.scroll = scroll;
    editor.autoscroll = false;

    let row_y = |row: usize| bounds.top() + lh * rows.shown(row) as f32 - scroll.y;
    let text_x = |x: Pixels| text_left + x - scroll.x;

    // Rectangles covering a byte range on the visible rows. A range that runs
    // past the end of a row shows its newline as half a cell.
    let range_rects = |r: Range<usize>| -> Vec<Bounds<Pixels>> {
        let start = buffer.offset_to_point(r.start);
        let end = buffer.offset_to_point(r.end);
        let mut rects = Vec::new();
        for row in start.row.max(first_row)..=end.row.min(end_row - 1) {
            let line = &lines[row - first_row];
            let x0 = if row == start.row {
                line.x_for(start.column)
            } else {
                px(0.)
            };
            let x1 = if row == end.row {
                line.x_for(end.column)
            } else {
                line.shaped.width + em * 0.5
            };
            if x1 > x0 {
                rects.push(Bounds::new(
                    point(text_x(x0), row_y(row)),
                    size(x1 - x0, lh),
                ));
            }
        }
        rects
    };

    let mut background = Vec::new();
    let mut selection_quads = Vec::new();
    let mut cursors = Vec::new();
    let mut cursor_rows = Vec::new();

    let first = editor
        .selections
        .partition_point(|s| s.range().end < visible.start);
    for s in &editor.selections[first..] {
        let r = s.range();
        if r.start > visible.end {
            break;
        }
        let head = buffer.offset_to_point(s.head);
        if head.row >= first_row && head.row < end_row {
            cursor_rows.push(head.row);
            let line = &lines[head.row - first_row];
            if focused {
                cursors.push(fill(
                    Bounds::new(
                        point(text_x(line.x_for(head.column)), row_y(head.row)),
                        size(px(2.), lh),
                    ),
                    theme.accent,
                ));
            }
            // One of many on a page (a cell of a notebook) marks the row
            // of its cursor only while the keys are its own.
            if s.is_empty() && !single_line && (focused || editor.fit.is_none()) {
                background.push(fill(
                    Bounds::new(
                        point(bounds.left(), row_y(head.row)),
                        size(bounds.size.width, lh),
                    ),
                    theme.active_line,
                ));
            }
        }
        if s.is_empty() {
            continue;
        }
        for rect in range_rects(r) {
            selection_quads.push(fill(rect, theme.selection));
        }
    }

    // The suggestion's first line continues the cursor's row; the rest are
    // drawn over the rows below, on the editor's background.
    let mut ghost = Vec::new();
    let mut ghost_background = Vec::new();
    if focused
        && editor.showing_ghost()
        && let Some(g) = &editor.ghost
    {
        let at = buffer.offset_to_point(g.offset);
        for (i, text) in g.text.split('\n').enumerate() {
            let row = at.row + i;
            if row < first_row || row >= end_row {
                if row >= end_row {
                    break;
                }
                continue;
            }
            let x = if i == 0 {
                text_x(lines[at.row - first_row].x_for(at.column))
            } else {
                ghost_background.push(fill(
                    Bounds::new(
                        point(text_bounds.left(), row_y(row)),
                        size(text_bounds.size.width, lh),
                    ),
                    theme.bg,
                ));
                text_x(px(0.))
            };
            if text.is_empty() {
                continue;
            }
            let text: SharedString = text.replace('\t', &" ".repeat(text::TAB_SIZE)).into();
            let run = TextRun {
                len: text.len(),
                font: code_font.clone(),
                color: theme.fg_subtle,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window
                .text_system()
                .shape_line(text, font_size, &[run], None);
            ghost.push((shaped, point(x, row_y(row))));
        }
    }

    let mut highlights = Vec::new();
    // A thin line down each level of indentation the rows on screen are
    // inside. A blank row is inside what the rows around it are.
    if settings.indent_guides && !single_line && !editor.masked {
        let unit = match doc.indent_unit() {
            "\t" => text::TAB_SIZE,
            spaces => spaces.len().max(1),
        };
        let levels = guide_levels(buffer, first_row..end_row, unit);
        for (row, levels) in (first_row..end_row).zip(levels) {
            // The row of its lenses is inside the same as the line.
            let shown = rows.shown(row);
            let lensed = shown > 0 && rows.line(shown - 1).1;
            let (top, tall) = match lensed {
                true => (row_y(row) - lh, lh * 2.),
                false => (row_y(row), lh),
            };
            for level in 0..levels {
                let x = text_x(em * (level * unit) as f32);
                if x >= text_left {
                    highlights.push(fill(
                        Bounds::new(point(x, top), size(theme.shape.border, tall)),
                        theme.line,
                    ));
                }
            }
        }
    }
    for decoration in doc
        .decorations()
        .iter()
        .take_while(|decoration| decoration.range.start <= visible.end)
    {
        if decoration.range.end < visible.start {
            continue;
        }
        if let Some(color) = theme.decoration(&decoration.background) {
            if decoration.whole_line {
                let start = buffer.offset_to_point(decoration.range.start).row;
                let end = buffer.offset_to_point(decoration.range.end).row;
                for row in start.max(first_row)..=end.min(end_row - 1) {
                    highlights.push(fill(
                        Bounds::new(point(text_left, row_y(row)), size(text_width, lh)),
                        color,
                    ));
                }
            } else {
                for rect in range_rects(decoration.range.clone()) {
                    highlights.push(fill(rect, color));
                }
            }
        }
    }
    let matches = editor.search_matches.clone();
    let first_match = matches.partition_point(|m| m.end < visible.start);
    for (i, m) in matches.iter().enumerate().skip(first_match) {
        if m.start > visible.end {
            break;
        }
        let color = if editor.active_match == Some(i) {
            theme.search_active
        } else {
            theme.search_match
        };
        for rect in range_rects(m.clone()) {
            highlights.push(fill(rect, color));
        }
    }
    if focused && !single_line && editor.selections.len() == 1 && editor.selections[0].is_empty() {
        // A language from an extension says in a query what its brackets
        // are, which may be words or tags. Any other is read by its
        // characters, as is a tree that is behind the text.
        let by_query = doc
            .syntax()
            .filter(|syntax| !syntax.is_stale())
            .and_then(|syntax| syntax.brackets_at(buffer.rope(), newest_head));
        let pair = match by_query {
            Some(pair) => pair,
            None => buffer
                .matching_bracket(newest_head, 20_000)
                .map(|(a, b)| (a..a + 1, b..b + 1)),
        };
        if let Some((open, close)) = pair {
            for range in [open, close] {
                for rect in range_rects(range) {
                    highlights.push(fill(rect, theme.bracket));
                }
            }
        }
    }
    let placeholder = match &editor.placeholder {
        Some(text) if buffer.is_empty() => {
            let run = TextRun {
                len: text.len(),
                font: code_font.clone(),
                color: theme.fg_subtle,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            Some(
                window
                    .text_system()
                    .shape_line(text.clone(), font_size, &[run], None),
            )
        }
        _ => None,
    };

    let mut gutter = Vec::with_capacity(lines.len());
    let number_right = text_left - GUTTER_PADDING;
    if !single_line {
        let mut marked_rows: Vec<(usize, Severity)> = Vec::new();
        for d in &visible_diagnostics {
            if d.severity > Severity::Warning {
                continue;
            }
            let row = buffer.offset_to_point(d.range.start).row;
            if row >= first_row && row < end_row && !marked_rows.iter().any(|(r, _)| *r == row) {
                marked_rows.push((row, d.severity));
            }
        }
        // Breakpoints, and where a debugged program is paused.
        let (breakpoints, paused_row, checked) =
            match (doc.path(), crate::debug::DebugStore::try_global(cx)) {
                (Some(path), Some(debug)) => {
                    let debug = debug.read(cx);
                    let lines: Vec<(usize, bool)> = debug
                        .lines(path)
                        .map(|l| {
                            l.iter()
                                .map(|(line, ok)| (*line as usize - 1, *ok))
                                .collect()
                        })
                        .unwrap_or_default();
                    let paused = debug
                        .paused
                        .as_ref()
                        .and_then(|p| p.frame())
                        .filter(|f| f.path.as_deref() == Some(path))
                        .map(|f| (f.line as usize).saturating_sub(1));
                    (lines, paused, debug.state.active())
                }
                _ => (Vec::new(), None, false),
            };
        if let Some(row) = paused_row.filter(|r| (first_row..end_row).contains(r)) {
            let mut tint = theme.warning;
            tint.a = 0.16;
            background.push(fill(
                Bounds::new(
                    point(bounds.left(), row_y(row)),
                    size(bounds.size.width, lh),
                ),
                tint,
            ));
            background.push(fill(
                Bounds::new(point(bounds.left(), row_y(row)), size(px(3.), lh)),
                theme.warning,
            ));
        }
        for (row, verified) in &breakpoints {
            if !(first_row..end_row).contains(row) {
                continue;
            }
            let size_px = (lh * 0.55).min(px(11.));
            let mut color = theme.error;
            // While a program runs, a breakpoint no session confirmed is faint.
            if checked && !verified {
                color.a = 0.45;
            }
            let mut quad = fill(
                Bounds::new(
                    point(bounds.left() + px(3.), row_y(*row) + (lh - size_px) / 2.),
                    size(size_px, size_px),
                ),
                color,
            );
            quad.corner_radii = gpui::Corners::all(size_px / 2.);
            background.push(quad);
        }
        let marked_rows: Vec<(usize, Severity)> = marked_rows
            .into_iter()
            .filter(|(row, _)| !breakpoints.iter().any(|(b, _)| b == row))
            .collect();
        for (row, severity) in marked_rows {
            let size_px = px(6.);
            let mut quad = fill(
                Bounds::new(
                    point(bounds.left() + px(6.), row_y(row) + (lh - size_px) / 2.),
                    size(size_px, size_px),
                ),
                severity_color(severity, theme),
            );
            quad.corner_radii = gpui::Corners::all(px(3.));
            background.push(quad);
        }
    }
    if !single_line {
        // Git: a bar per changed row range, a notch where lines were deleted.
        let visible_rows = first_row..end_row;
        for hunk in doc.hunks().iter() {
            if hunk.is_deletion() {
                if visible_rows.contains(&hunk.new.start) || hunk.new.start == end_row {
                    background.push(fill(
                        Bounds::new(
                            point(bounds.left(), row_y(hunk.new.start) - px(1.5)),
                            size(px(8.), px(3.)),
                        ),
                        theme.error,
                    ));
                }
                continue;
            }
            let start = hunk.new.start.max(first_row);
            let end = hunk.new.end.min(end_row);
            if start >= end {
                continue;
            }
            let color = if hunk.is_insertion() {
                theme.git_added
            } else {
                theme.git_modified
            };
            background.push(fill(
                Bounds::new(
                    point(bounds.left() + px(1.), row_y(start)),
                    size(px(3.), row_y(end - 1) + lh - row_y(start)),
                ),
                color,
            ));
        }
        // Merge conflicts: tint each side across the full width.
        for conflict in doc.conflicts().iter() {
            let sides = [
                (conflict.start..conflict.middle, theme.conflict_ours),
                (conflict.middle + 1..conflict.end + 1, theme.conflict_theirs),
            ];
            for (rows, color) in sides {
                let start = rows.start.max(first_row);
                let end = rows.end.min(end_row);
                if start < end {
                    background.push(fill(
                        Bounds::new(
                            point(bounds.left(), row_y(start)),
                            size(bounds.size.width, row_y(end - 1) + lh - row_y(start)),
                        ),
                        color,
                    ));
                }
            }
        }
    }
    for row in (first_row..end_row).filter(|_| !single_line) {
        let active = cursor_rows.contains(&row);
        let label: SharedString = (row + 1).to_string().into();
        let run = TextRun {
            len: label.len(),
            font: code_font.clone(),
            color: if active {
                theme.fg_muted
            } else {
                theme.fg_subtle
            },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window
            .text_system()
            .shape_line(label, font_size, &[run], None);
        let origin = point(number_right - shaped.width, row_y(row));
        gutter.push((shaped, origin));
    }

    PrepaintState {
        layout: Some(LayoutSnapshot {
            bounds,
            text_left,
            line_height: lh,
            em_width: em,
            first_row,
            lines,
            rows,
            lens_rows,
        }),
        started: Instant::now(),
        scroll,
        text_bounds,
        background,
        highlights,
        selections: selection_quads,
        placeholder,
        cursors,
        ghost,
        ghost_background,
        gutter,
        reveal,
    }
}

/// How far from a blank row the rows that say how deep it is are looked for.
const GUIDE_REACH: usize = 200;

/// How many levels of indentation each row of a range is inside, where a
/// level is `unit` cells. A blank row is as deep as the deeper of the
/// rows with text around it: it is inside what goes on past it.
fn guide_levels(buffer: &Buffer, rows: Range<usize>, unit: usize) -> Vec<usize> {
    // Blank rows at the edge of the screen look past it, not far.
    let around = |row: usize, step: isize| -> usize {
        let mut at = row as isize + step;
        for _ in 0..GUIDE_REACH {
            if at < 0 || at as usize >= buffer.line_count() {
                break;
            }
            if let Some(cells) = indent_cells(&buffer.line_str(at as usize)) {
                return cells;
            }
            at += step;
        }
        0
    };
    rows.map(|row| {
        let cells = match indent_cells(&buffer.line_str(row)) {
            Some(cells) => cells,
            None => around(row, -1).max(around(row, 1)),
        };
        cells / unit.max(1)
    })
    .collect()
}

/// How far in a row's text begins, in cells; nothing for a blank row.
fn indent_cells(line: &str) -> Option<usize> {
    let mut cells = 0;
    for c in line.chars() {
        match c {
            ' ' => cells += 1,
            '\t' => cells += text::TAB_SIZE - cells % text::TAB_SIZE,
            _ => return Some(cells),
        }
    }
    None
}

fn severity_color(severity: Severity, theme: &Theme) -> gpui::Hsla {
    match severity {
        Severity::Error => theme.error,
        Severity::Warning => theme.warning,
        Severity::Info | Severity::Hint => theme.fg_subtle,
    }
}

fn clamp(v: Pixels, lo: Pixels, hi: Pixels) -> Pixels {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

fn highlights(
    editor: &mut Editor,
    doc: &Document,
    range: Range<usize>,
) -> Arc<Vec<(Range<usize>, HighlightKind)>> {
    let key = (
        doc.version(),
        doc.syntax_generation(),
        doc.semantic_generation(),
        range.clone(),
    );
    if let Some(cache) = &editor.highlight_cache
        && cache.key == key
    {
        return cache.spans.clone();
    }
    let from_grammar = doc
        .syntax()
        .map(|tree| tree.highlights(doc.text().rope(), range.clone()))
        .unwrap_or_default();
    let spans = Arc::new(overlaid(from_grammar, doc.semantic(), &range));
    editor.highlight_cache = Some(HighlightCache {
        key,
        spans: spans.clone(),
    });
    spans
}

/// The grammar's colors with a language server's over them: where the
/// server says what a word is, its word wins, and the grammar keeps the
/// rest. Both come sorted, and the answer is.
fn overlaid(
    base: Vec<(Range<usize>, HighlightKind)>,
    over: &[(Range<usize>, HighlightKind)],
    visible: &Range<usize>,
) -> Vec<(Range<usize>, HighlightKind)> {
    let over = &over[over.partition_point(|(range, _)| range.end <= visible.start)..];
    let over = &over[..over.partition_point(|(range, _)| range.start < visible.end)];
    if over.is_empty() {
        return base;
    }
    let mut out = Vec::with_capacity(base.len() + over.len());
    for (range, kind) in base {
        let mut start = range.start;
        let first = over.partition_point(|(covering, _)| covering.end <= start);
        for (covering, _) in &over[first..] {
            if covering.start >= range.end {
                break;
            }
            if covering.start > start {
                out.push((start..covering.start, kind));
            }
            start = start.max(covering.end);
        }
        if start < range.end {
            out.push((start..range.end, kind));
        }
    }
    out.extend(over.iter().cloned());
    out.sort_by_key(|(range, _)| range.start);
    out
}

/// Splits a row into `(byte range within the row, color)` segments covering it
/// completely. Returns the index of the first span that may touch later rows.
fn color_segments(
    text: &str,
    line_start: usize,
    spans: &[(Range<usize>, HighlightKind)],
    mut ix: usize,
    theme: &Theme,
) -> (Vec<(Range<usize>, gpui::Hsla)>, usize) {
    let line_end = line_start + text.len();
    let mut segments = Vec::new();
    let mut pos = 0;
    while let Some((r, kind)) = spans.get(ix) {
        if r.start >= line_end {
            break;
        }
        let s = r.start.max(line_start) - line_start;
        let e = r.end.min(line_end) - line_start;
        if s > pos {
            segments.push((pos..s, theme.fg));
        }
        if e > s.max(pos) {
            segments.push((s.max(pos)..e, theme.syntax.color(*kind)));
            pos = e;
        }
        if r.end > line_end {
            // Continues on the next row (block comment, template string).
            break;
        }
        ix += 1;
    }
    if pos < text.len() {
        segments.push((pos..text.len(), theme.fg));
    }
    (segments, ix)
}

fn shape_row(
    text: &str,
    line_start: usize,
    segments: Vec<(Range<usize>, gpui::Hsla)>,
    underlines: &[(Range<usize>, UnderlineStyle)],
    (hints, hint_color): (&[(usize, &str)], gpui::Hsla),
    (code_font, font_size): (&gpui::Font, Pixels),
    window: &mut Window,
) -> DisplayLine {
    // Expand tabs to the next stop so columns line up with `display_column`,
    // and put each hint before the character it stands at.
    let mut tabs = Vec::new();
    let mut inlays = Vec::new();
    let expanded: SharedString = if text.contains('\t') || !hints.is_empty() {
        let mut out = String::with_capacity(text.len() + 16);
        let mut col = 0;
        let mut hints = hints.iter().peekable();
        for (i, g) in text.grapheme_indices(true) {
            while let Some((_, hint)) = hints.next_if(|(at, _)| *at <= i) {
                out.push_str(hint);
                inlays.push((i, hint.len()));
            }
            if g == "\t" {
                let width = text::TAB_SIZE - col % text::TAB_SIZE;
                out.extend(std::iter::repeat_n(' ', width));
                tabs.push((i, width - 1));
                col += width;
            } else {
                out.push_str(g);
                col += 1;
            }
        }
        // The ones at the end of the row.
        for (_, hint) in hints {
            out.push_str(hint);
            inlays.push((text.len(), hint.len()));
        }
        out.into()
    } else {
        SharedString::from(text.to_owned())
    };
    let line = DisplayLine {
        shaped: ShapedLine::default(),
        tabs,
        inlays,
        len: text.len(),
    };

    // Underlines (IME composition, diagnostics) in row-relative bytes. The
    // first one listed wins where they overlap.
    let line_end = line_start + text.len();
    let underlines: Vec<(Range<usize>, UnderlineStyle)> = underlines
        .iter()
        .filter(|(r, _)| r.end > line_start && r.start < line_end)
        .map(|(r, style)| {
            (
                r.start.saturating_sub(line_start)..(r.end - line_start).min(text.len()),
                *style,
            )
        })
        .collect();
    let mut runs = Vec::with_capacity(segments.len() + 2);
    let hint_run = |bytes: usize| TextRun {
        len: bytes,
        font: code_font.clone(),
        color: hint_color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    // How many of the row's hints have a run already.
    let mut hinted = 0;
    for (range, color) in segments {
        let mut cuts = vec![range.start, range.end];
        for (u, _) in &underlines {
            for c in [u.start, u.end] {
                if c > range.start && c < range.end {
                    cuts.push(c);
                }
            }
        }
        // A hint inside a word of one color parts it in two.
        cuts.extend(
            line.inlays
                .iter()
                .map(|(at, _)| *at)
                .filter(|at| *at > range.start && *at < range.end),
        );
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            // The hints that stand at `a` come first, in their own color.
            while let Some((_, bytes)) = line.inlays.get(hinted).filter(|(at, _)| *at <= a) {
                runs.push(hint_run(*bytes));
                hinted += 1;
            }
            let underline = underlines
                .iter()
                .find(|(u, _)| a >= u.start && b <= u.end)
                .map(|(_, style)| UnderlineStyle {
                    color: style.color.or(Some(color)),
                    ..*style
                });
            runs.push(TextRun {
                len: line.expand(b) - line.expand_after(a),
                font: code_font.clone(),
                color,
                background_color: None,
                underline,
                strikethrough: None,
            });
        }
    }
    for (_, bytes) in &line.inlays[hinted..] {
        runs.push(hint_run(*bytes));
    }
    let shaped = window
        .text_system()
        .shape_line(expanded, font_size, &runs, None);
    DisplayLine { shaped, ..line }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(tabs: &[(usize, usize)], inlays: &[(usize, usize)], len: usize) -> DisplayLine {
        DisplayLine {
            shaped: ShapedLine::default(),
            tabs: tabs.to_vec(),
            inlays: inlays.to_vec(),
            len,
        }
    }

    #[test]
    fn rows_are_inside_the_levels_they_are_indented_to() {
        let text = "fn a() {\n    if b {\n\n        c();\n    }\n\n}\n\tx\n  \n";
        let buffer = Buffer::new(text);
        // The blank row inside the `if` is as deep as what follows it;
        // the one after its `}` is as deep as that; a tab is one level
        // of four cells, and a row of spaces only is blank, as deep as
        // the row with text nearest to it.
        assert_eq!(
            guide_levels(&buffer, 0..buffer.line_count(), 4),
            [0, 1, 2, 2, 1, 1, 0, 1, 1, 1]
        );
        // Asked for some rows only, it still looks at the ones around.
        assert_eq!(guide_levels(&buffer, 2..3, 4), [2]);
        assert_eq!(guide_levels(&buffer, 3..4, 2), [4]);
    }

    #[test]
    fn lens_rows_map_both_ways_without_changing_file_lines() {
        let rows = Rows(Arc::new(vec![0, 2, 3, 90]));
        let expected = [
            (0, true),
            (0, false),
            (1, false),
            (2, true),
            (2, false),
            (3, true),
            (3, false),
            (4, false),
        ];
        for (shown, expected) in expected.into_iter().enumerate() {
            assert_eq!(rows.line(shown), expected);
        }
        for line in 0..100 {
            assert_eq!(rows.line(rows.shown(line)), (line, false));
        }
        assert_eq!(Rows::default().line(42), (42, false));
    }

    #[test]
    fn a_hint_in_a_row_moves_what_is_after_it_and_is_no_column() {
        // `let a = f(b)` with `: i32` before the space at 5 and `x: `
        // before the `b` at 10.
        let line = row(&[], &[(5, 5), (10, 3)], 12);
        // The cursor at a hint's column stands before the hint.
        assert_eq!(line.expand(5), 5);
        assert_eq!(line.expand_after(5), 10);
        assert_eq!(line.expand(6), 11);
        assert_eq!(line.expand(10), 15);
        assert_eq!(line.expand(12), 20);
        // A place before, in and after a hint is a column of the file.
        assert_eq!(line.collapse(4), 4);
        for inside in 5..10 {
            assert_eq!(line.collapse(inside), 5, "{inside}");
        }
        assert_eq!(line.collapse(10), 5);
        assert_eq!(line.collapse(11), 6);
        for inside in 15..18 {
            assert_eq!(line.collapse(inside), 10);
        }
        assert_eq!(line.collapse(19), 11);
        assert_eq!(line.collapse(99), 12);
        // Every column goes there and back.
        for col in 0..=12 {
            assert_eq!(line.collapse(line.expand(col)), col);
        }

        // With a tab before the hints (three spaces more at column 0), a
        // hint at the tab's own column, and one at the end of the row.
        let line = row(&[(0, 3)], &[(0, 2), (4, 2)], 4);
        assert_eq!(line.expand(0), 0);
        assert_eq!(line.expand(1), 6);
        assert_eq!(line.expand(4), 9);
        assert_eq!(line.expand_after(4), 11);
        assert_eq!((line.collapse(1), line.collapse(2)), (0, 0));
        // In the spaces of the tab: the nearer side of it.
        assert_eq!((line.collapse(3), line.collapse(5)), (0, 1));
        assert_eq!(
            (line.collapse(6), line.collapse(9), line.collapse(11)),
            (1, 4, 4)
        );
        // A row with neither is itself.
        let plain = row(&[], &[], 3);
        assert_eq!(
            (plain.expand(2), plain.collapse(2), plain.collapse(9)),
            (2, 2, 3)
        );
    }

    #[test]
    fn a_server_s_colors_go_over_the_grammar_s() {
        use HighlightKind::*;
        let grammar = vec![(0..2, Keyword), (3..9, Variable), (10..20, Comment)];
        // Nothing from the server on screen: the grammar's as they are.
        assert_eq!(overlaid(grammar.clone(), &[], &(0..20)), grammar);
        assert_eq!(
            overlaid(grammar.clone(), &[(30..40, Type)], &(0..20)),
            grammar
        );
        // The server's word wins where it is, whole or part of a span,
        // and the grammar keeps the rest on both sides.
        let server = [(3..9, Function), (12..14, Type), (16..18, Constant)];
        assert_eq!(
            overlaid(grammar.clone(), &server, &(0..20)),
            [
                (0..2, Keyword),
                (3..9, Function),
                (10..12, Comment),
                (12..14, Type),
                (14..16, Comment),
                (16..18, Constant),
                (18..20, Comment),
            ]
        );
        // One over two of the grammar's, and where the grammar has none.
        assert_eq!(
            overlaid(
                vec![(0..4, Keyword), (4..8, String)],
                &[(2..6, Type), (9..11, Number)],
                &(0..20)
            ),
            [
                (0..2, Keyword),
                (2..6, Type),
                (6..8, String),
                (9..11, Number)
            ]
        );
    }
}
