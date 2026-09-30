//! Integrated terminal: Alacritty's emulator and PTY, drawn on the GPU by
//! Solder's own element. Only the visible grid is shaped each frame.

use std::{borrow::Cow, collections::HashMap, ops::Range, path::PathBuf, sync::Arc};

use alacritty_terminal::{
    event::{Event as TermEvent, EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point as GridPoint, Side},
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config, Term, TermMode, cell::Flags},
    tty,
    vte::ansi::{Color, CursorShape, NamedColor, Rgb},
};
use futures::StreamExt;
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, FontWeight, GlobalElementId,
    Hsla, InspectorElementId, IntoElement, KeyBinding, KeyDownEvent, Keystroke, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, PaintQuad, Pixels, Point, Render,
    ScrollWheelEvent, ShapedLine, SharedString, Style, Task, TextRun, UTF16Selection, Window,
    actions, div, fill, point, prelude::*, px, relative, rgb, size,
};

use crate::{settings::Settings, theme::ActiveTheme};

actions!(terminal, [Copy, Paste, Clear]);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("Terminal");
    cx.bind_keys([
        KeyBinding::new("secondary-c", Copy, ctx),
        KeyBinding::new("secondary-v", Paste, ctx),
        KeyBinding::new("secondary-k", Clear, ctx),
    ]);
}

pub enum TerminalEvent {
    TitleChanged,
    /// The shell exited; the tab can close.
    Exited,
}

impl EventEmitter<TerminalEvent> for Terminal {}

#[derive(Clone)]
struct Listener(futures::channel::mpsc::UnboundedSender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        self.0.unbounded_send(event).ok();
    }
}

struct GridSize {
    columns: usize,
    lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// What to run. `None` program means the user's login shell.
#[derive(Clone, Default)]
pub struct TerminalCommand {
    pub program: Option<String>,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub title: Option<String>,
    /// Keep the tab after the process exits, so its output can be read
    /// (services that crash).
    pub keep_on_exit: bool,
}

pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    focus_handle: FocusHandle,
    title: SharedString,
    fixed_title: bool,
    pub keep_on_exit: bool,
    /// The process has exited; `exit_code` is `None` when a signal ended it.
    pub exited: bool,
    pub exit_code: Option<i32>,
    grid: (usize, usize),
    cell: (Pixels, Pixels),
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    _events: Task<()>,
}

/// A started PTY and emulator, before it becomes a view.
pub struct StartedTerminal {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    events: futures::channel::mpsc::UnboundedReceiver<TermEvent>,
    title: String,
    fixed_title: bool,
    keep_on_exit: bool,
}

impl Terminal {
    /// Starts the shell (or `command.program`) in a new PTY.
    pub fn start(command: TerminalCommand) -> std::io::Result<StartedTerminal> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let mut env = command.env.clone();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "Solder".into());
        let options = tty::Options {
            shell: command
                .program
                .clone()
                .map(|p| tty::Shell::new(p, command.args.clone())),
            working_directory: Some(command.cwd.clone()),
            drain_on_exit: true,
            env,
            #[cfg(target_os = "windows")]
            escape_args: true,
        };
        let window_size = WindowSize {
            num_lines: 24,
            num_cols: 80,
            cell_width: 8,
            cell_height: 16,
        };
        let pty = tty::new(&options, window_size, 0)?;
        let term = Term::new(
            Config::default(),
            &GridSize {
                columns: 80,
                lines: 24,
            },
            Listener(tx.clone()),
        );
        let term = Arc::new(FairMutex::new(term));
        let event_loop = EventLoop::new(term.clone(), Listener(tx), pty, true, false)?;
        let sender = event_loop.channel();
        let _io_thread = event_loop.spawn();
        let fixed_title = command.title.is_some();
        let title = command.title.unwrap_or_else(|| {
            command
                .program
                .as_deref()
                .and_then(|p| p.rsplit('/').next())
                .unwrap_or("Terminal")
                .to_string()
        });
        Ok(StartedTerminal {
            term,
            sender,
            events: rx,
            title,
            fixed_title,
            keep_on_exit: command.keep_on_exit,
        })
    }

    pub fn new(started: StartedTerminal, cx: &mut Context<Self>) -> Self {
        let StartedTerminal {
            term,
            sender,
            events: mut rx,
            title,
            fixed_title,
            keep_on_exit,
        } = started;
        let events = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut batch = vec![first];
                while let Ok(e) = rx.try_recv() {
                    batch.push(e);
                }
                let alive = this
                    .update(cx, |this, cx| {
                        for e in batch {
                            this.handle_event(e, cx);
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });

        Self {
            term,
            sender,
            focus_handle: cx.focus_handle(),
            title: title.into(),
            fixed_title,
            keep_on_exit,
            exited: false,
            exit_code: None,
            grid: (80, 24),
            cell: (px(8.), px(16.)),
            last_bounds: None,
            selecting: false,
            _events: events,
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// Ctrl+C: asks the foreground process to stop.
    pub fn interrupt(&self) {
        self.write(&b"\x03"[..]);
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    /// Visible text, one string per row. For tests and "copy all".
    pub fn visible_text(&self) -> Vec<String> {
        let term = self.term.lock();
        let content = term.renderable_content();
        let mut rows: Vec<String> = vec![String::new(); term.screen_lines()];
        let offset = content.display_offset as i32;
        for cell in content.display_iter {
            let row = (cell.point.line.0 + offset) as usize;
            if let Some(r) = rows.get_mut(row)
                && !cell.flags.contains(Flags::WIDE_CHAR_SPACER)
            {
                r.push(cell.c);
            }
        }
        rows.into_iter().map(|r| r.trim_end().to_string()).collect()
    }

    fn handle_event(&mut self, event: TermEvent, cx: &mut Context<Self>) {
        match event {
            TermEvent::Title(title) if !self.fixed_title => {
                self.title = title.into();
                cx.emit(TerminalEvent::TitleChanged);
            }
            TermEvent::PtyWrite(text) => self.write(text.into_bytes()),
            TermEvent::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            TermEvent::ClipboardLoad(_, format) => {
                let text = cx
                    .read_from_clipboard()
                    .and_then(|i| i.text())
                    .unwrap_or_default();
                self.write(format(&text).into_bytes());
            }
            TermEvent::TextAreaSizeRequest(format) => {
                let size = self.window_size();
                self.write(format(size).into_bytes());
            }
            // Both events can arrive for one exit; report it once.
            TermEvent::ChildExit(status) if !self.exited => {
                self.exited = true;
                self.exit_code = status.code();
                cx.emit(TerminalEvent::Exited);
            }
            TermEvent::Exit if !self.exited => {
                self.exited = true;
                cx.emit(TerminalEvent::Exited);
            }
            _ => {}
        }
    }

    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.grid.1 as u16,
            num_cols: self.grid.0 as u16,
            cell_width: f32::from(self.cell.0) as u16,
            cell_height: f32::from(self.cell.1) as u16,
        }
    }

    fn resize(&mut self, columns: usize, lines: usize, cell: (Pixels, Pixels)) {
        if (columns, lines) == self.grid && cell == self.cell {
            return;
        }
        self.grid = (columns, lines);
        self.cell = cell;
        self.term.lock().resize(GridSize { columns, lines });
        let _ = self.sender.send(Msg::Resize(self.window_size()));
    }

    fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    // ------------------------------------------------------------ input

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let mode = self.mode();
        if let Some(bytes) = escape_sequence(&event.keystroke, mode.contains(TermMode::APP_CURSOR))
        {
            self.scroll_to_bottom();
            self.term.lock().selection = None;
            self.write(bytes);
            cx.stop_propagation();
        }
    }

    fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.term.lock().selection_to_string() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
            return;
        };
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            self.write(format!("\x1b[200~{text}\x1b[201~").into_bytes());
        } else {
            self.write(text.into_bytes());
        }
        self.scroll_to_bottom();
    }

    fn clear(&mut self, _: &Clear, _: &mut Window, cx: &mut Context<Self>) {
        // Form feed: the shell redraws its prompt at the top.
        self.write(&b"\x0c"[..]);
        cx.notify();
    }

    fn cell_at(&self, position: Point<Pixels>) -> Option<(GridPoint, Side)> {
        let bounds = self.last_bounds?;
        let x = (position.x - bounds.left()).max(px(0.));
        let y = (position.y - bounds.top()).max(px(0.));
        let col = ((x / self.cell.0) as usize).min(self.grid.0.saturating_sub(1));
        let row = ((y / self.cell.1) as usize).min(self.grid.1.saturating_sub(1));
        let side = if (x / self.cell.0).fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let offset = self.term.lock().grid().display_offset() as i32;
        Some((GridPoint::new(Line(row as i32 - offset), Column(col)), side))
    }

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        let Some((point, side)) = self.cell_at(e.position) else {
            return;
        };
        let kind = match e.click_count {
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        self.term.lock().selection = Some(Selection::new(kind, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || e.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            return;
        }
        let Some((point, side)) = self.cell_at(e.position) else {
            return;
        };
        if let Some(selection) = self.term.lock().selection.as_mut() {
            selection.update(point, side);
        }
        cx.notify();
    }

    fn scroll(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = e.delta.pixel_delta(self.cell.1).y;
        let lines = (delta / self.cell.1).round() as i32;
        if lines == 0 {
            return;
        }
        let mode = self.mode();
        if mode.contains(TermMode::ALT_SCREEN) && !mode.intersects(TermMode::MOUSE_MODE) {
            // Full-screen apps (less, vim) scroll with arrow keys.
            let key: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
            for _ in 0..lines.abs() {
                self.write(key.to_vec());
            }
        } else {
            self.term.lock().scroll_display(Scroll::Delta(lines));
        }
        cx.notify();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

/// Bytes a terminal sends for a keystroke that is not plain text.
/// Plain characters come through the input handler instead (so IME works).
pub fn escape_sequence(k: &Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
    let m = &k.modifiers;
    if m.platform {
        return None;
    }
    let modifier_code = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control);
    let csi = |final_byte: &str| -> Vec<u8> {
        if modifier_code > 1 {
            format!("\x1b[1;{modifier_code}{final_byte}").into_bytes()
        } else if app_cursor && matches!(final_byte, "A" | "B" | "C" | "D" | "H" | "F") {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modifier_code > 1 {
            format!("\x1b[{n};{modifier_code}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let bytes = match k.key.as_str() {
        "enter" => {
            if m.alt {
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" if m.control => b"\x08".to_vec(),
        "backspace" if m.alt => b"\x1b\x7f".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "space" if m.control => vec![0],
        "up" => csi("A"),
        "down" => csi("B"),
        "right" => csi("C"),
        "left" => csi("D"),
        "home" => csi("H"),
        "end" => csi("F"),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        key if m.control && key.len() == 1 => {
            let c = key.as_bytes()[0].to_ascii_lowercase();
            let code = match c {
                b'a'..=b'z' => c - b'a' + 1,
                b'@' | b'2' => 0,
                b'[' | b'3' => 0x1b,
                b'\\' | b'4' => 0x1c,
                b']' | b'5' => 0x1d,
                b'^' | b'6' => 0x1e,
                b'_' | b'-' | b'7' => 0x1f,
                _ => return None,
            };
            if m.alt { vec![0x1b, code] } else { vec![code] }
        }
        // Option as Meta: Esc prefix, the base key rather than the symbol
        // macOS would type.
        key if m.alt && key.chars().count() == 1 => {
            let mut v = vec![0x1b];
            let c = if m.shift {
                key.to_uppercase()
            } else {
                key.to_string()
            };
            v.extend_from_slice(c.as_bytes());
            v
        }
        _ => return None,
    };
    Some(bytes)
}

impl EntityInputHandler for Terminal {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::perf::Perf::input_started(cx);
        self.scroll_to_bottom();
        self.term.lock().selection = None;
        self.write(text.replace('\n', "\r").into_bytes());
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        // Composition is shown by the OS candidate window; the shell only
        // receives the committed text.
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Focusable for Terminal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Terminal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .px_2()
            .py_1()
            .bg(cx.theme().bg)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::clear))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(TerminalElement {
                terminal: cx.entity(),
            })
    }
}

// ---------------------------------------------------------------- colors

/// ANSI palette in the site's zinc tones, plus the 256-color cube.
fn palette_color(index: usize, dark: bool) -> Hsla {
    const DARK: [u32; 16] = [
        0x27272a, 0xf87171, 0x86efac, 0xfbbf24, 0x93c5fd, 0xd8b4fe, 0x67e8f9, 0xd4d4d8, 0x52525b,
        0xfca5a5, 0xbbf7d0, 0xfde68a, 0xbfdbfe, 0xe9d5ff, 0xa5f3fc, 0xfafafa,
    ];
    const LIGHT: [u32; 16] = [
        0x18181b, 0xdc2626, 0x15803d, 0xb45309, 0x1d4ed8, 0x7e22ce, 0x0e7490, 0x52525b, 0x71717a,
        0xef4444, 0x16a34a, 0xca8a04, 0x2563eb, 0x9333ea, 0x0891b2, 0x27272a,
    ];
    match index {
        0..16 => rgb(if dark { DARK[index] } else { LIGHT[index] }).into(),
        16..232 => {
            let i = index - 16;
            let level = |v: usize| if v == 0 { 0 } else { 55 + v as u32 * 40 };
            let (r, g, b) = (level(i / 36), level((i / 6) % 6), level(i % 6));
            rgb((r << 16) | (g << 8) | b).into()
        }
        _ => {
            let v = 8 + (index.min(255) - 232) as u32 * 10;
            rgb((v << 16) | (v << 8) | v).into()
        }
    }
}

fn resolve_color(
    color: Color,
    overrides: &alacritty_terminal::term::color::Colors,
    fg: Hsla,
    bg: Hsla,
    dark: bool,
) -> Hsla {
    let from_rgb =
        |c: Rgb| -> Hsla { rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32).into() };
    match color {
        Color::Spec(c) => from_rgb(c),
        Color::Indexed(i) => {
            overrides[i as usize].map_or_else(|| palette_color(i as usize, dark), from_rgb)
        }
        Color::Named(named) => {
            if let Some(c) = overrides[named as usize] {
                return from_rgb(c);
            }
            match named {
                NamedColor::Foreground | NamedColor::BrightForeground => fg,
                NamedColor::Background => bg,
                NamedColor::Cursor => fg,
                NamedColor::DimForeground => fg.opacity(0.7),
                n => {
                    let i = n as usize;
                    if i < 16 {
                        palette_color(i, dark)
                    } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize)
                        .contains(&i)
                    {
                        palette_color(i - NamedColor::DimBlack as usize, dark).opacity(0.7)
                    } else {
                        fg
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- element

/// A cell ready to draw: character, foreground, background, flags, column.
type GridCell = (char, Hsla, Hsla, Flags, Column);

struct TerminalElement {
    terminal: Entity<Terminal>,
}

struct TerminalPrepaint {
    rows: Vec<ShapedLine>,
    backgrounds: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
    cell_height: Pixels,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPrepaint;

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
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
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
    ) -> TerminalPrepaint {
        let settings = Settings::get(cx).clone();
        let theme = cx.theme().clone();
        let dark = theme.bg.l < 0.5;
        let focused = self.terminal.read(cx).focus_handle.is_focused(window);
        let font = settings.buffer_font();
        let font_size = settings.buffer_font_size();
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .map_or(px(8.), |s| s.width);
        let cell_height = (font_size * 1.35).round();
        let columns = ((bounds.size.width / cell_width) as usize).max(2);
        let lines = ((bounds.size.height / cell_height) as usize).max(1);

        self.terminal.update(cx, |t, _| {
            t.last_bounds = Some(bounds);
            t.resize(columns, lines, (cell_width, cell_height));
        });

        let terminal = self.terminal.read(cx);
        let term = terminal.term.lock();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let selection = content.selection;
        let colors = content.colors;
        let fg_default = theme.fg;
        let bg_default = theme.bg;

        // Collect cells per visible row.
        let mut rows: Vec<Vec<GridCell>> = vec![Vec::new(); lines];
        for cell in content.display_iter {
            let row = (cell.point.line.0 + offset) as usize;
            let Some(r) = rows.get_mut(row) else { continue };
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let (mut fg, mut bg) = (
                resolve_color(cell.fg, colors, fg_default, bg_default, dark),
                resolve_color(cell.bg, colors, fg_default, bg_default, dark),
            );
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = fg.opacity(0.7);
            }
            let c = if cell.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            };
            r.push((c, fg, bg, cell.flags, cell.point.column));
        }

        let origin = bounds.origin;
        let mut backgrounds = Vec::new();
        let mut shaped = Vec::with_capacity(lines);
        for (row, cells) in rows.iter().enumerate() {
            let y = origin.y + cell_height * row as f32;
            // Merge runs of the same background into one quad.
            let mut run_start: Option<(usize, Hsla)> = None;
            let flush = |start: usize, end: usize, color: Hsla, out: &mut Vec<PaintQuad>| {
                if color != bg_default {
                    out.push(fill(
                        Bounds::new(
                            point(origin.x + cell_width * start as f32, y),
                            size(cell_width * (end - start) as f32, cell_height),
                        ),
                        color,
                    ));
                }
            };
            for (i, (_, _, bg, _, col)) in cells.iter().enumerate() {
                let col = col.0;
                match run_start {
                    Some((_, color)) if color == *bg => {}
                    Some((start, color)) => {
                        flush(start, col, color, &mut backgrounds);
                        run_start = Some((col, *bg));
                    }
                    None => run_start = Some((col, *bg)),
                }
                if i == cells.len() - 1
                    && let Some((start, color)) = run_start
                {
                    flush(start, col + 1, color, &mut backgrounds);
                }
            }
            // Selection highlight.
            if let Some(sel) = selection {
                let line = Line(row as i32 - offset);
                if sel.start.line <= line && line <= sel.end.line {
                    let first = if sel.start.line == line {
                        sel.start.column.0
                    } else {
                        0
                    };
                    let last = if sel.end.line == line {
                        sel.end.column.0
                    } else {
                        columns - 1
                    };
                    if last >= first {
                        backgrounds.push(fill(
                            Bounds::new(
                                point(origin.x + cell_width * first as f32, y),
                                size(cell_width * (last - first + 1) as f32, cell_height),
                            ),
                            theme.selection,
                        ));
                    }
                }
            }
            // Text: one run per change of color or weight.
            let mut text = String::with_capacity(cells.len());
            let mut runs: Vec<TextRun> = Vec::new();
            for (c, fg, _, flags, _) in cells {
                let start = text.len();
                text.push(*c);
                let len = text.len() - start;
                let weight = if flags.contains(Flags::BOLD) {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                };
                match runs.last_mut() {
                    Some(r) if r.color == *fg && r.font.weight == weight => r.len += len,
                    _ => runs.push(TextRun {
                        len,
                        font: gpui::Font {
                            weight,
                            ..font.clone()
                        },
                        color: *fg,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }),
                }
            }
            shaped.push(window.text_system().shape_line(
                text.into(),
                font_size,
                &runs,
                Some(cell_width),
            ));
        }

        let cursor = {
            let c = content.cursor;
            let row = c.point.line.0 + offset;
            (row >= 0 && (row as usize) < lines && c.shape != CursorShape::Hidden).then(|| {
                let x = origin.x + cell_width * c.point.column.0 as f32;
                let y = origin.y + cell_height * row as f32;
                let (w, h, dy) = match (c.shape, focused) {
                    (CursorShape::Beam, _) => (px(2.), cell_height, px(0.)),
                    (CursorShape::Underline, _) => (cell_width, px(2.), cell_height - px(2.)),
                    (_, true) => (cell_width, cell_height, px(0.)),
                    (_, false) => (cell_width, cell_height, px(0.)),
                };
                if focused || c.shape != CursorShape::Block {
                    fill(
                        Bounds::new(point(x, y + dy), size(w, h)),
                        theme.accent.opacity(0.75),
                    )
                } else {
                    gpui::outline(
                        Bounds::new(point(x, y), size(w, h)),
                        theme.accent,
                        gpui::BorderStyle::Solid,
                    )
                }
            })
        };
        drop(term);

        TerminalPrepaint {
            rows: shaped,
            backgrounds,
            cursor,
            cell_height,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut TerminalPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.terminal.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.terminal.clone()),
            cx,
        );
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in state.backgrounds.drain(..) {
                window.paint_quad(quad);
            }
            for (row, line) in state.rows.iter().enumerate() {
                let origin = point(bounds.left(), bounds.top() + state.cell_height * row as f32);
                line.paint(origin, state.cell_height, window, cx).ok();
            }
            if let Some(cursor) = state.cursor.take() {
                window.paint_quad(cursor);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn key(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn keys_map_to_escape_sequences() {
        assert_eq!(escape_sequence(&key("enter"), false).unwrap(), b"\r");
        assert_eq!(escape_sequence(&key("up"), false).unwrap(), b"\x1b[A");
        assert_eq!(escape_sequence(&key("up"), true).unwrap(), b"\x1bOA");
        assert_eq!(
            escape_sequence(&key("shift-right"), false).unwrap(),
            b"\x1b[1;2C"
        );
        assert_eq!(escape_sequence(&key("ctrl-c"), false).unwrap(), b"\x03");
        assert_eq!(escape_sequence(&key("alt-b"), false).unwrap(), b"\x1bb");
        assert_eq!(
            escape_sequence(&key("pagedown"), false).unwrap(),
            b"\x1b[6~"
        );
        assert!(escape_sequence(&key("cmd-c"), false).is_none());
        assert!(escape_sequence(&key("a"), false).is_none());
        let _ = Modifiers::default();
    }

    #[test]
    fn palette_covers_all_indices() {
        assert_eq!(palette_color(1, true), rgb(0xf87171).into());
        assert_eq!(palette_color(16, true), rgb(0x000000).into());
        assert_eq!(palette_color(231, true), rgb(0xffffff).into());
        assert_eq!(palette_color(255, true), rgb(0xeeeeee).into());
    }
}
