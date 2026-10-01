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

/// A shaped row plus the bookkeeping to map buffer columns to shaped columns.
pub struct DisplayLine {
    pub shaped: ShapedLine,
    /// `(byte column of a tab, extra bytes it expanded into)`, in order.
    tabs: Vec<(usize, usize)>,
    /// Byte length of the row that was shaped (rows can be truncated).
    len: usize,
}

impl DisplayLine {
    fn expand(&self, col: usize) -> usize {
        let col = col.min(self.len);
        col + self
            .tabs
            .iter()
            .take_while(|(at, _)| *at < col)
            .map(|(_, extra)| extra)
            .sum::<usize>()
    }

    fn collapse(&self, expanded: usize) -> usize {
        let mut shift = 0;
        for (at, extra) in &self.tabs {
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
}

impl LayoutSnapshot {
    fn row_at(&self, buffer: &Buffer, scroll: Point<Pixels>, y: Pixels) -> usize {
        let row = ((y - self.bounds.top() + scroll.y) / self.line_height).floor();
        (row.max(0.) as usize).min(buffer.line_count() - 1)
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
        let row_f = (position.y - self.bounds.top() + scroll.y) / self.line_height;
        let row = row_f.floor() as usize;
        if row >= buffer.line_count() {
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
        let y = self.bounds.top() + self.line_height * p.row as f32 - scroll.y;
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
        style.size.height = if single_line {
            Settings::get(cx).line_height().into()
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
                    let origin = point(
                        layout.text_left - scroll.x,
                        bounds.top() + layout.line_height * row as f32 - scroll.y,
                    );
                    line.shaped
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
    if editor.autoscroll {
        let margin = if single_line {
            px(0.)
        } else {
            (lh * 3.).min(height / 3.)
        };
        let top = lh * cursor_row as f32;
        if top - margin < scroll.y {
            scroll.y = top - margin;
        } else if top + lh + margin > scroll.y + height {
            scroll.y = top + lh + margin - height;
        }
    }
    let max_y = lh * line_count.saturating_sub(1) as f32;
    scroll.y = clamp(scroll.y, px(0.), max_y);

    let first_row = ((scroll.y / lh).floor() as usize).min(line_count - 1);
    let end_row = (((scroll.y + height) / lh).ceil() as usize + 1).min(line_count);
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
        lines.push(shape_row(
            text,
            line_start,
            segments,
            &underlines,
            &code_font,
            font_size,
            window,
        ));
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
        .fold(px(0.), |a, b| if b > a { b } else { a });
    let max_x = widest + em * 2. - text_width;
    scroll.x = clamp(
        scroll.x,
        px(0.),
        if max_x > px(0.) { max_x } else { px(0.) },
    );
    editor.scroll = scroll;
    editor.autoscroll = false;

    let row_y = |row: usize| bounds.top() + lh * row as f32 - scroll.y;
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
            if s.is_empty() && !single_line {
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
    if focused
        && !single_line
        && editor.selections.len() == 1
        && editor.selections[0].is_empty()
        && let Some((a, b)) = buffer.matching_bracket(newest_head, 20_000)
    {
        for at in [a, b] {
            for rect in range_rects(at..at + 1) {
                highlights.push(fill(rect, theme.bracket));
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
                    size(px(3.), lh * (end - start) as f32),
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
                            size(bounds.size.width, lh * (end - start) as f32),
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
    }
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
    let key = (doc.version(), doc.syntax_generation(), range.clone());
    if let Some(cache) = &editor.highlight_cache
        && cache.key == key
    {
        return cache.spans.clone();
    }
    let spans = Arc::new(
        doc.syntax()
            .map(|tree| tree.highlights(doc.text().rope(), range))
            .unwrap_or_default(),
    );
    editor.highlight_cache = Some(HighlightCache {
        key,
        spans: spans.clone(),
    });
    spans
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
    code_font: &gpui::Font,
    font_size: Pixels,
    window: &mut Window,
) -> DisplayLine {
    // Expand tabs to the next stop so columns line up with `display_column`.
    let mut tabs = Vec::new();
    let expanded: SharedString = if text.contains('\t') {
        let mut out = String::with_capacity(text.len() + 16);
        let mut col = 0;
        for (i, g) in text.grapheme_indices(true) {
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
        out.into()
    } else {
        SharedString::from(text.to_owned())
    };
    let line = DisplayLine {
        shaped: ShapedLine::default(),
        tabs,
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
    for (range, color) in segments {
        let mut cuts = vec![range.start, range.end];
        for (u, _) in &underlines {
            for c in [u.start, u.end] {
                if c > range.start && c < range.end {
                    cuts.push(c);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let underline = underlines
                .iter()
                .find(|(u, _)| a >= u.start && b <= u.end)
                .map(|(_, style)| UnderlineStyle {
                    color: style.color.or(Some(color)),
                    ..*style
                });
            runs.push(TextRun {
                len: line.expand(b) - line.expand(a),
                font: code_font.clone(),
                color,
                background_color: None,
                underline,
                strikethrough: None,
            });
        }
    }
    let shaped = window
        .text_system()
        .shape_line(expanded, font_size, &runs, None);
    DisplayLine { shaped, ..line }
}
