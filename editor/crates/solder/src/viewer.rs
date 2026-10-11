//! What is shown and not typed into: a picture, and Markdown as it reads.
//!
//! A viewer is a tab of a pane, next to the tabs of files. A picture file
//! opens as one. A preview is asked for from a Markdown or SVG file and
//! follows its document, saved or not: what is typed is what is shown.

use crate::lang::{t, tf};
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Font, FontStyle,
    FontWeight, Hsla, Image, ImageFormat, InteractiveText, ListAlignment, ListState, ObjectFit,
    SharedString, StrikethroughStyle, StyledText, Subscription, Task, TextRun, UnderlineStyle,
    Window, div, img, list, prelude::*, px,
};
use syntax::HighlightKind;

use crate::{
    document::{Document, DocumentEvent},
    markdown::{self, Align, Block, Inline, Item},
    settings::Settings,
    theme::{self, ActiveTheme, Theme, UI_FONT_SIZE, UI_FONT_SMALL},
};

/// How long after the last key a preview is made again.
const SETTLE: Duration = Duration::from_millis(120);
/// A picture larger than this is not read into memory to be shown.
const PICTURE_LIMIT: u64 = 64 * 1024 * 1024;

/// The format of a picture file, by the name it ends with.
pub fn picture_format(path: &Path) -> Option<ImageFormat> {
    let ending = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ending.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        "svg" => ImageFormat::Svg,
        _ => return None,
    })
}

/// Whether a file opens as a picture and not as text. An SVG is text
/// first: it is written by hand, and has a preview like Markdown.
pub fn is_picture(path: &Path) -> bool {
    picture_format(path).is_some_and(|format| format != ImageFormat::Svg)
}

/// Whether a file has a preview: Markdown, or an SVG.
pub fn has_preview(path: &Path) -> bool {
    let ending = path
        .extension()
        .and_then(|ending| ending.to_str())
        .map(str::to_ascii_lowercase);
    matches!(
        ending.as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd" | "svg")
    )
}

/// How wide and tall a picture is, read from the first bytes of its file.
pub fn picture_size(format: ImageFormat, bytes: &[u8]) -> Option<(u32, u32)> {
    let be = |at: usize, len: usize| -> Option<u32> {
        let bytes = bytes.get(at..at + len)?;
        Some(bytes.iter().fold(0, |n, byte| n << 8 | u32::from(*byte)))
    };
    let le = |at: usize, len: usize| -> Option<u32> {
        let bytes = bytes.get(at..at + len)?;
        Some(
            bytes
                .iter()
                .rev()
                .fold(0, |n, byte| n << 8 | u32::from(*byte)),
        )
    };
    match format {
        ImageFormat::Png if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => {
            Some((be(16, 4)?, be(20, 4)?))
        }
        ImageFormat::Gif if bytes.starts_with(b"GIF8") => Some((le(6, 2)?, le(8, 2)?)),
        ImageFormat::Bmp if bytes.starts_with(b"BM") => {
            // The height is negative for rows stored from the top.
            Some((le(18, 4)?, (le(22, 4)? as i32).unsigned_abs()))
        }
        ImageFormat::Jpeg if bytes.starts_with(&[0xff, 0xd8]) => {
            // Segments follow one another until the one that starts the
            // picture, which says how large it is.
            let mut at = 2;
            loop {
                if *bytes.get(at)? != 0xff {
                    return None;
                }
                let marker = *bytes.get(at + 1)?;
                match marker {
                    0xff => at += 1,
                    0xd0..=0xd9 | 0x01 => at += 2,
                    0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                        return Some((be(at + 7, 2)?, be(at + 5, 2)?));
                    }
                    _ => at += 2 + be(at + 2, 2)? as usize,
                }
            }
        }
        ImageFormat::Webp if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => {
            match bytes.get(12..16)? {
                b"VP8X" => Some((le(24, 3)? + 1, le(27, 3)? + 1)),
                b"VP8 " => Some((le(26, 2)? & 0x3fff, le(28, 2)? & 0x3fff)),
                b"VP8L" => {
                    let bits = le(21, 4)?;
                    Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// A size of a file as people say it.
fn size_words(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Webp => "WebP",
        ImageFormat::Gif => "GIF",
        ImageFormat::Svg => "SVG",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Tiff => "TIFF",
    }
}

struct Picture {
    format: ImageFormat,
    image: Option<Arc<Image>>,
    size: Option<(u32, u32)>,
    bytes: usize,
    /// Why there is nothing to show.
    failed: Option<SharedString>,
}

/// A block of a preview with what was worked out for drawing it: for
/// code, what each part of it is in its language.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    line: usize,
    block: Block,
    colors: Vec<(Range<usize>, HighlightKind)>,
    /// How large each picture of the block is, by the address it is
    /// written with: a picture is given its room before it is read.
    sizes: Vec<(String, (u32, u32))>,
}

struct Preview {
    rows: Arc<Vec<Row>>,
    list: ListState,
    /// How wide the page was when it was last drawn: a picture is given
    /// no more. The list cannot be asked while it lays its rows out.
    width: f32,
}

enum Kind {
    Picture(Picture),
    Markdown(Preview),
}

pub enum ViewerEvent {
    /// A link to a file of this machine was clicked.
    Open(PathBuf),
    /// What is shown changed its name or its state.
    Changed,
}

pub struct Viewer {
    /// The picture's file, or the file a preview is of.
    pub path: PathBuf,
    /// The document a preview follows. A picture file has none.
    source: Option<Entity<Document>>,
    kind: Kind,
    focus: FocusHandle,
    work: Option<Task<()>>,
    _watch: Option<Subscription>,
}

impl EventEmitter<ViewerEvent> for Viewer {}

impl Focusable for Viewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Viewer {
    /// A picture file, read off the thread that draws.
    pub fn picture(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let format = picture_format(&path).unwrap_or(ImageFormat::Png);
        let mut viewer = Self {
            path,
            source: None,
            kind: Kind::Picture(Picture {
                format,
                image: None,
                size: None,
                bytes: 0,
                failed: None,
            }),
            focus: cx.focus_handle(),
            work: None,
            _watch: None,
        };
        viewer.reload(cx);
        viewer
    }

    /// What `document` reads as, kept up with what is typed into it.
    pub fn preview(document: Entity<Document>, cx: &mut Context<Self>) -> Self {
        let path = document
            .read(cx)
            .path()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let kind = match picture_format(&path) {
            Some(format) => Kind::Picture(Picture {
                format,
                image: None,
                size: None,
                bytes: 0,
                failed: None,
            }),
            None => Kind::Markdown(Preview {
                rows: Arc::default(),
                list: ListState::new(0, ListAlignment::Top, px(600.)),
                width: 600.,
            }),
        };
        let watch = cx.subscribe(&document, |this, _, event, cx| match event {
            DocumentEvent::Edited { .. } => this.follow(SETTLE, cx),
            DocumentEvent::PathChanged => {
                if let Some(path) = this.source.as_ref().and_then(|d| d.read(cx).path()) {
                    this.path = path.to_path_buf();
                }
                cx.emit(ViewerEvent::Changed);
                cx.notify();
            }
            _ => {}
        });
        let mut viewer = Self {
            path,
            source: Some(document),
            kind,
            focus: cx.focus_handle(),
            work: None,
            _watch: Some(watch),
        };
        viewer.follow(Duration::ZERO, cx);
        viewer
    }

    /// How wide a page of Markdown is now: the list less its margins, and
    /// never wider than a line reads well. Nothing before it was drawn.
    fn page_width(&self) -> Option<f32> {
        let Kind::Markdown(preview) = &self.kind else {
            return None;
        };
        let wide = f32::from(preview.list.viewport_bounds().size.width);
        (wide > 96.).then(|| (wide - 48.).min(860.))
    }

    /// The folder the links and pictures of a page are looked for from.
    fn base(&self) -> PathBuf {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    pub fn is_preview_of(&self, document: &Entity<Document>) -> bool {
        self.source.as_ref() == Some(document)
    }

    pub fn is_preview(&self) -> bool {
        self.source.is_some()
    }

    /// What the tab says.
    pub fn title(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into());
        match self.source {
            Some(_) => tf("Preview {}", &[&name]),
            None => name,
        }
    }

    /// Reads a picture's file again: at first, and when it changed.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.source.is_some() {
            return;
        }
        let Kind::Picture(picture) = &self.kind else {
            return;
        };
        let (path, format) = (self.path.clone(), picture.format);
        let read = cx.background_executor().spawn(async move {
            let length = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            if length > PICTURE_LIMIT {
                return Err(t("This picture is too large to show").to_string());
            }
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            Ok((picture_size(format, &bytes), bytes))
        });
        self.work = Some(cx.spawn(async move |this, cx| {
            let read = read.await;
            this.update(cx, |this, cx| {
                match read {
                    Ok((size, bytes)) => this.show_picture(size, bytes, cx),
                    Err(why) => {
                        if let Kind::Picture(picture) = &mut this.kind {
                            picture.failed = Some(why.into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn show_picture(&mut self, size: Option<(u32, u32)>, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let Kind::Picture(picture) = &mut self.kind else {
            return;
        };
        // What was drawn of the bytes before is not kept for a picture
        // that is no longer shown.
        if let Some(old) = picture.image.take() {
            old.remove_asset(cx);
        }
        picture.failed = None;
        picture.size = size;
        picture.bytes = bytes.len();
        picture.image = Some(Arc::new(Image::from_bytes(picture.format, bytes)));
    }

    /// Makes the preview again from what its document holds now, once the
    /// document has been still for `after`.
    fn follow(&mut self, after: Duration, cx: &mut Context<Self>) {
        let Some(document) = self.source.clone() else {
            return;
        };
        self.work = Some(cx.spawn(async move |this, cx| {
            if !after.is_zero() {
                cx.background_executor().timer(after).await;
            }
            let Ok(text) = document.read_with(cx, |document, _| document.text().rope().clone())
            else {
                return;
            };
            let picture = this
                .read_with(cx, |this, _| matches!(this.kind, Kind::Picture(_)))
                .unwrap_or(false);
            if picture {
                let bytes = text.to_string().into_bytes();
                this.update(cx, |this, cx| {
                    this.show_picture(None, bytes, cx);
                    cx.notify();
                })
                .ok();
                return;
            }
            let base = this
                .read_with(cx, |this, _| this.base())
                .unwrap_or_default();
            let rows = cx
                .background_executor()
                .spawn(async move { rows(&text.to_string(), &base) })
                .await;
            this.update(cx, |this, cx| {
                if let Kind::Markdown(preview) = &mut this.kind {
                    preview.put(rows);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    #[cfg(test)]
    pub fn blocks(&self) -> Vec<Block> {
        match &self.kind {
            Kind::Markdown(preview) => preview.rows.iter().map(|row| row.block.clone()).collect(),
            Kind::Picture(_) => Vec::new(),
        }
    }

    /// What is known of the picture: its size, its bytes, why it failed.
    #[cfg(test)]
    pub fn picture_state(&self) -> Option<PictureState> {
        match &self.kind {
            Kind::Picture(picture) => Some((
                picture.size,
                picture.bytes,
                picture.failed.as_ref().map(|why| why.to_string()),
            )),
            Kind::Markdown(_) => None,
        }
    }

    /// Follows a link of the preview: a file of this machine opens in the
    /// editor, anything else where the system opens addresses.
    pub(crate) fn follow_link(&self, to: &str, cx: &mut Context<Self>) {
        if to.starts_with('#') {
            return;
        }
        if to.contains("://") || to.starts_with("mailto:") {
            cx.open_url(to);
            return;
        }
        let file = to.split(['#', '?']).next().unwrap_or(to);
        let path = match Path::new(file).is_absolute() {
            true => PathBuf::from(file),
            false => self.path.parent().unwrap_or(Path::new("")).join(file),
        };
        if path.is_file() {
            cx.emit(ViewerEvent::Open(path));
        }
    }
}

/// The addresses of the pictures in `inlines`.
fn pictured<'a>(inlines: &'a [Inline], out: &mut Vec<&'a str>) {
    for inline in inlines {
        match inline {
            Inline::Image { from, .. } => out.push(from),
            Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                pictured(inner, out)
            }
            Inline::Link { text, .. } => pictured(text, out),
            _ => {}
        }
    }
}

/// The file an address of a page in `base` names, if it names one of
/// this machine.
fn local(base: &Path, from: &str) -> Option<PathBuf> {
    let file = from.split(['#', '?']).next().unwrap_or(from);
    (!from.contains("://") && !from.starts_with("data:") && !file.is_empty())
        .then(|| base.join(file))
}

/// How large a picture file is, from no more of it than says so.
fn size_of_file(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let format = picture_format(path)?;
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(512 * 1024)
        .read_to_end(&mut head)
        .ok()?;
    picture_size(format, &head)
}

/// The blocks of a Markdown text in the folder `base`, with the colors
/// of its code and the sizes of its pictures. It reads files and loads
/// grammars: not for the thread that draws.
fn rows(text: &str, base: &Path) -> Vec<Row> {
    markdown::parse(text)
        .into_iter()
        .map(|(line, block)| {
            let mut colors = Vec::new();
            let mut sizes = Vec::new();
            match &block {
                Block::Code { language, text } if !language.is_empty() => {
                    colors = syntax::language_named(language)
                        .map(|language| syntax::highlight_code(&language, text))
                        .unwrap_or_default();
                }
                Block::Paragraph(inlines) => {
                    let mut from = Vec::new();
                    pictured(inlines, &mut from);
                    for from in from {
                        let size = local(base, from).and_then(|path| size_of_file(&path));
                        if let Some(size) = size.filter(|(w, h)| *w > 0 && *h > 0) {
                            sizes.push((from.to_string(), size));
                        }
                    }
                }
                _ => {}
            }
            Row {
                line,
                block,
                colors,
                sizes,
            }
        })
        .collect()
}

impl Preview {
    /// Takes the blocks as they are now. Only the ones that changed are
    /// measured again, so that a preview scrolled to one place stays there
    /// while another is typed into.
    fn put(&mut self, rows: Vec<Row>) {
        let same =
            |a: &Row, b: &Row| a.block == b.block && a.colors == b.colors && a.sizes == b.sizes;
        let old = &self.rows;
        let head = old
            .iter()
            .zip(&rows)
            .take_while(|(a, b)| same(a, b))
            .count();
        let tail = old[head..]
            .iter()
            .rev()
            .zip(rows[head..].iter().rev())
            .take_while(|(a, b)| same(a, b))
            .count();
        self.list
            .splice(head..old.len() - tail, rows.len() - head - tail);
        self.rows = Arc::new(rows);
    }
}

/// What draws the text of a preview: the fonts and colors in use, and a
/// counter for the names clickable text needs.
struct Pen<'a> {
    theme: &'a Theme,
    font: Font,
    code: Font,
    /// The folder pictures and links are looked for from.
    base: PathBuf,
    /// How large the pictures of the block are, and how wide the page.
    sizes: &'a [(String, (u32, u32))],
    width: f32,
    viewer: gpui::WeakEntity<Viewer>,
    row: usize,
    next: std::cell::Cell<usize>,
}

/// A line of text as it is drawn: the text, a run for each stretch set
/// the same way, and the stretches that are links with where they lead.
type Flat = (String, Vec<TextRun>, Vec<(Range<usize>, String)>);

/// What is known of a picture: its size, its bytes, why it failed.
#[cfg(test)]
pub type PictureState = (Option<(u32, u32)>, usize, Option<String>);

#[derive(Clone, Copy, Default)]
struct Face {
    strong: bool,
    slanted: bool,
    struck: bool,
    code: bool,
    link: Option<usize>,
    faint: bool,
}

impl Pen<'_> {
    /// The text of `inlines` as one piece, with a run for each stretch
    /// set the same way and the stretches that are links.
    fn flatten(&self, inlines: &[Inline], face: Face, color: Hsla, out: &mut Flat) {
        for inline in inlines {
            let (words, face): (&str, Face) = match inline {
                Inline::Text(text) => (text, face),
                Inline::Code(text) => (text, Face { code: true, ..face }),
                Inline::Break => ("\n", face),
                Inline::Image { alt, .. } => (
                    alt,
                    Face {
                        faint: true,
                        slanted: true,
                        ..face
                    },
                ),
                Inline::Emphasis(inner) => {
                    self.flatten(
                        inner,
                        Face {
                            slanted: true,
                            ..face
                        },
                        color,
                        out,
                    );
                    continue;
                }
                Inline::Strong(inner) => {
                    self.flatten(
                        inner,
                        Face {
                            strong: true,
                            ..face
                        },
                        color,
                        out,
                    );
                    continue;
                }
                Inline::Strike(inner) => {
                    self.flatten(
                        inner,
                        Face {
                            struck: true,
                            ..face
                        },
                        color,
                        out,
                    );
                    continue;
                }
                Inline::Link { text, to } => {
                    let start = out.0.len();
                    let link = Some(out.2.len());
                    out.2.push((start..start, to.clone()));
                    self.flatten(text, Face { link, ..face }, color, out);
                    if let Some((range, _)) = link.and_then(|link| out.2.get_mut(link)) {
                        range.end = out.0.len();
                    }
                    continue;
                }
            };
            if words.is_empty() {
                continue;
            }
            let mut font = if face.code {
                self.code.clone()
            } else {
                self.font.clone()
            };
            if face.strong {
                font.weight = FontWeight::BOLD;
            }
            if face.slanted {
                font.style = FontStyle::Italic;
            }
            let color = match (face.link.is_some(), face.faint) {
                (true, _) => self.theme.accent,
                (false, true) => self.theme.fg_subtle,
                (false, false) => color,
            };
            out.0.push_str(words);
            out.1.push(TextRun {
                len: words.len(),
                font,
                color,
                background_color: face.code.then_some(self.theme.bg_sunken),
                underline: face.link.is_some().then_some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(self.theme.accent),
                    wavy: false,
                }),
                strikethrough: face.struck.then_some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color),
                }),
            });
        }
    }

    fn text(&self, inlines: &[Inline], face: Face, color: Hsla) -> AnyElement {
        let mut out = (String::new(), Vec::new(), Vec::new());
        self.flatten(inlines, face, color, &mut out);
        let (text, runs, links) = out;
        if text.is_empty() {
            return div().into_any_element();
        }
        let styled = StyledText::new(SharedString::from(text)).with_runs(runs);
        let id = self.next.replace(self.next.get() + 1);
        let (ranges, targets): (Vec<_>, Vec<_>) = links.into_iter().unzip();
        let viewer = self.viewer.clone();
        InteractiveText::new(("preview-text", self.row * 4096 + id), styled)
            .on_click(ranges, move |link, _, cx| {
                if let Some(to) = targets.get(link) {
                    viewer
                        .update(cx, |viewer, cx| viewer.follow_link(to, cx))
                        .ok();
                }
            })
            .into_any_element()
    }

    /// The pictures of a paragraph that is nothing but pictures, with the
    /// address each one leads to if it is a link.
    fn pictures<'a>(
        &self,
        inlines: &'a [Inline],
    ) -> Option<Vec<(&'a str, &'a str, Option<&'a str>)>> {
        let mut found = Vec::new();
        for inline in inlines {
            match inline {
                Inline::Image { alt, from } => found.push((alt.as_str(), from.as_str(), None)),
                Inline::Link { text, to } => match text.as_slice() {
                    [Inline::Image { alt, from }] => {
                        found.push((alt.as_str(), from.as_str(), Some(to.as_str())))
                    }
                    _ => return None,
                },
                Inline::Text(text) if text.trim().is_empty() => {}
                Inline::Break => {}
                _ => return None,
            }
        }
        (!found.is_empty()).then_some(found)
    }

    /// A picture of the page: drawn if its file is on this machine, named
    /// if it is somewhere the editor does not go by itself.
    fn picture(&self, alt: &str, from: &str, to: Option<&str>) -> AnyElement {
        // A picture whose size was read is a file that is there.
        let size = self.sizes.iter().find(|(known, _)| known == from);
        let svg = local(&self.base, from)
            .filter(|path| picture_format(path) == Some(ImageFormat::Svg) && size.is_none());
        let id = self.next.replace(self.next.get() + 1);
        let shown = match (size, svg) {
            (Some((_, (wide, tall))), _) => {
                // As large as it is, and no larger than the page or
                // than a picture should stand in a text.
                let (wide, tall) = (*wide as f32, *tall as f32);
                let scale = (self.width / wide).min(480. / tall).min(1.);
                img(self
                    .base
                    .join(from.split(['#', '?']).next().unwrap_or(from)))
                .w(px((wide * scale).round()))
                .h(px((tall * scale).round()))
                .flex_none()
                .into_any_element()
            }
            (None, Some(path)) => img(path)
                .max_w_full()
                .max_h(px(480.))
                .object_fit(ObjectFit::ScaleDown)
                .into_any_element(),
            (None, None) => div()
                .px_1p5()
                .rounded(self.theme.shape.token)
                .border(self.theme.shape.border)
                .border_color(self.theme.line)
                .text_size(UI_FONT_SMALL)
                .text_color(self.theme.fg_subtle)
                .child(match alt.is_empty() {
                    true => t("picture").to_string(),
                    false => alt.to_string(),
                })
                .into_any_element(),
        };
        let to = to.map(str::to_string);
        let viewer = self.viewer.clone();
        div()
            .id(("preview-picture", self.row * 4096 + id))
            .max_w_full()
            .child(shown)
            .when_some(to, |d, to| {
                d.cursor_pointer().on_click(move |_, _, cx| {
                    viewer
                        .update(cx, |viewer, cx| viewer.follow_link(&to, cx))
                        .ok();
                })
            })
            .into_any_element()
    }

    fn code(&self, text: &str, colors: &[(Range<usize>, HighlightKind)]) -> AnyElement {
        let mut runs = Vec::new();
        let mut at = 0;
        let run = |len: usize, color: Hsla| TextRun {
            len,
            font: self.code.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        // What the language says of a place may be said twice: as in the
        // editor, the last to say it is what it is.
        let mut kinds: Vec<Option<HighlightKind>> = vec![None; text.len()];
        for (range, kind) in colors {
            if let Some(place) = kinds.get_mut(range.clone()) {
                place.fill(Some(*kind));
            }
        }
        while at < text.len() {
            let kind = kinds[at];
            let mut end = at + 1;
            while end < text.len() && (kinds[end] == kind || !text.is_char_boundary(end)) {
                end += 1;
            }
            let color = kind.map_or(self.theme.fg, |kind| self.theme.syntax.color(kind));
            runs.push(run(end - at, color));
            at = end;
        }
        let id = self.next.replace(self.next.get() + 1);
        div()
            .id(("preview-code", self.row * 4096 + id))
            .w_full()
            .px_3()
            .py_2()
            .rounded(self.theme.shape.control)
            .bg(self.theme.bg_sunken)
            .overflow_x_scroll()
            .text_size(theme::text(12.5))
            .child(match text.is_empty() {
                true => div().into_any_element(),
                false => div()
                    .whitespace_nowrap()
                    .child(StyledText::new(SharedString::from(text.to_string())).with_runs(runs))
                    .into_any_element(),
            })
            .into_any_element()
    }

    fn blocks(&self, blocks: &[Block], color: Hsla) -> Vec<AnyElement> {
        blocks
            .iter()
            .map(|block| self.block(block, &[], color))
            .collect()
    }

    fn block(
        &self,
        block: &Block,
        colors: &[(Range<usize>, HighlightKind)],
        color: Hsla,
    ) -> AnyElement {
        let theme = self.theme;
        match block {
            Block::Heading { level, text } => {
                let size = match level {
                    1 => 24.,
                    2 => 20.,
                    3 => 17.,
                    4 => 15.,
                    _ => 14.,
                };
                div()
                    .w_full()
                    .pt_2()
                    .text_size(theme::text(size))
                    .when(*level <= 2, |d| {
                        d.pb_1()
                            .border_b(theme.shape.border)
                            .border_color(theme.line)
                    })
                    .child(self.text(
                        text,
                        Face {
                            strong: true,
                            ..Face::default()
                        },
                        color,
                    ))
                    .into_any_element()
            }
            Block::Paragraph(inlines) => match self.pictures(inlines) {
                Some(pictures) => div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1p5()
                    .children(
                        pictures
                            .into_iter()
                            .map(|(alt, from, to)| self.picture(alt, from, to)),
                    )
                    .into_any_element(),
                None => div()
                    .w_full()
                    .child(self.text(inlines, Face::default(), color))
                    .into_any_element(),
            },
            Block::Code { text, .. } => self.code(text, colors),
            Block::Html(text) => div()
                .w_full()
                .text_size(theme::text(12.5))
                .text_color(theme.fg_subtle)
                .font(self.code.clone())
                .child(text.clone())
                .into_any_element(),
            Block::Rule => div()
                .w_full()
                .h(theme.shape.border)
                .my_1()
                .bg(theme.line)
                .into_any_element(),
            Block::Quote(inner) => div()
                .w_full()
                .pl_3()
                .border_l_2()
                .border_color(theme.line)
                .flex()
                .flex_col()
                .gap_2()
                .children(self.blocks(inner, theme.fg_muted))
                .into_any_element(),
            Block::List { first, items } => div()
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .children(items.iter().enumerate().map(|(ix, Item { done, blocks })| {
                    let mark = match (done, first) {
                        (Some(true), _) => "☑".to_string(),
                        (Some(false), _) => "☐".to_string(),
                        (None, Some(first)) => format!("{}.", first + ix as u64),
                        (None, None) => "•".to_string(),
                    };
                    div()
                        .w_full()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(18.))
                                .text_color(theme.fg_subtle)
                                .child(mark),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(self.blocks(blocks, color)),
                        )
                }))
                .into_any_element(),
            Block::Table { head, align, rows } => {
                let row = |cells: &[Vec<Inline>], head: bool| {
                    div()
                        .w_full()
                        .flex()
                        .border_b(theme.shape.border)
                        .border_color(theme.line)
                        .when(head, |d| d.bg(theme.bg_elev))
                        .children(cells.iter().zip(align).map(|(cell, align)| {
                            let face = Face {
                                strong: head,
                                ..Face::default()
                            };
                            div()
                                .flex_1()
                                .min_w_0()
                                .px_2()
                                .py_1()
                                .flex()
                                .map(|d| match align {
                                    Align::Left => d.justify_start(),
                                    Align::Center => d.justify_center(),
                                    Align::Right => d.justify_end(),
                                })
                                .child(self.text(cell, face, color))
                        }))
                };
                div()
                    .w_full()
                    .border(theme.shape.border)
                    .border_color(theme.line)
                    .rounded(theme.shape.token)
                    .overflow_hidden()
                    .child(row(head, true))
                    .children(rows.iter().map(|cells| row(cells, false)))
                    .into_any_element()
            }
        }
    }
}

impl Viewer {
    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Kind::Markdown(preview) = &self.kind else {
            return div().into_any_element();
        };
        let Some(row) = preview.rows.get(ix) else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let pen = Pen {
            theme: &theme,
            font: window.text_style().font(),
            code: Settings::get(cx).buffer_font(),
            base: self.base(),
            sizes: &row.sizes,
            width: preview.width,
            viewer: cx.weak_entity(),
            row: ix,
            next: Default::default(),
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .px_6()
            .pb_3()
            .when(ix == 0, |d| d.pt_4())
            .child(div().w_full().max_w(px(860.)).child(pen.block(
                &row.block,
                &row.colors,
                theme.fg,
            )))
            .into_any_element()
    }

    fn render_picture(&self, picture: &Picture, theme: &Theme) -> AnyElement {
        let said = |words: SharedString| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(UI_FONT_SIZE)
                .text_color(theme.fg_subtle)
                .child(words)
                .into_any_element()
        };
        let Some(image) = &picture.image else {
            return said(
                picture
                    .failed
                    .clone()
                    .unwrap_or_else(|| t("Reading the picture").into()),
            );
        };
        let mut facts = Vec::new();
        if let Some((width, height)) = picture.size {
            facts.push(format!("{width} × {height}"));
        }
        facts.push(format_name(picture.format).to_string());
        facts.push(size_words(picture.bytes));
        let subtle = theme.fg_subtle;
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        img(image.clone())
                            .max_w_full()
                            .max_h_full()
                            .object_fit(ObjectFit::ScaleDown)
                            .with_fallback(move || {
                                div()
                                    .text_size(UI_FONT_SIZE)
                                    .text_color(subtle)
                                    .child(t("This picture could not be read"))
                                    .into_any_element()
                            }),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "picture-facts".into())
                    .flex_none()
                    .px_3()
                    .py_1()
                    .border_t(theme.shape.border)
                    .border_color(theme.line)
                    .text_size(UI_FONT_SMALL)
                    .text_color(theme.fg_subtle)
                    .child(facts.join("  ·  ")),
            )
            .into_any_element()
    }
}

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if let Some(width) = self.page_width()
            && let Kind::Markdown(preview) = &mut self.kind
        {
            preview.width = width;
        }
        // The list is laid out after this, and may come out another
        // width: the first time, and when the window is sized. The rows
        // are then drawn once more for the width it has.
        let viewer = cx.weak_entity();
        window.on_next_frame(move |_, cx| {
            viewer
                .update(cx, |this, cx| {
                    let drawn = match &this.kind {
                        Kind::Markdown(preview) => Some(preview.width),
                        Kind::Picture(_) => None,
                    };
                    if this.page_width().is_some_and(|now| Some(now) != drawn) {
                        cx.notify();
                    }
                })
                .ok();
        });
        let body = match &self.kind {
            Kind::Picture(picture) => self.render_picture(picture, &theme),
            Kind::Markdown(preview) if preview.rows.is_empty() => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(UI_FONT_SIZE)
                .text_color(theme.fg_subtle)
                .child(t("Nothing to show yet"))
                .into_any_element(),
            Kind::Markdown(preview) => {
                let row =
                    cx.processor(|this, ix: usize, window, cx| this.render_row(ix, window, cx));
                list(preview.list.clone(), row)
                    .size_full()
                    .into_any_element()
            }
        };
        div()
            .id("viewer")
            .key_context("Viewer")
            .track_focus(&self.focus)
            .size_full()
            .bg(theme.bg)
            .text_color(theme.fg)
            .text_size(theme::text(14.))
            .line_height(gpui::relative(1.5))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_says_how_large_it_is() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend([0, 0, 2, 128, 0, 0, 1, 224]);
        assert_eq!(picture_size(ImageFormat::Png, &png), Some((640, 480)));
        assert_eq!(
            picture_size(ImageFormat::Gif, b"GIF89a\x40\x01\xf0\x00"),
            Some((320, 240))
        );
        let mut bmp = vec![0; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[18..22].copy_from_slice(&800u32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-600i32).to_le_bytes());
        assert_eq!(picture_size(ImageFormat::Bmp, &bmp), Some((800, 600)));
        // A segment of something else, then the one that starts the picture.
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0, 4, 1, 2, 0xff, 0xc2, 0, 11, 8, 0x04, 0x38, 0x07, 0x80,
        ];
        assert_eq!(picture_size(ImageFormat::Jpeg, &jpeg), Some((1920, 1080)));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        webp.extend([0xff, 0x03, 0, 0xff, 0x01, 0]);
        assert_eq!(picture_size(ImageFormat::Webp, &webp), Some((1024, 512)));
        // What is cut short or is something else has no size, and no panic.
        for bytes in [
            &b""[..],
            b"\x89PNG",
            b"GIF8",
            b"BM",
            &[0xff, 0xd8, 0xff],
            b"RIFFxxxxWEBP",
        ] {
            for format in [
                ImageFormat::Png,
                ImageFormat::Gif,
                ImageFormat::Bmp,
                ImageFormat::Jpeg,
                ImageFormat::Webp,
            ] {
                assert_eq!(picture_size(format, bytes), None);
            }
        }
    }

    #[test]
    fn code_of_a_known_language_has_colors() {
        let made = rows(
            "```rust\nfn main() { let n = 1; }\n```\n\n```nothing\nx\n```\n",
            Path::new(""),
        );
        assert!(!made[0].colors.is_empty());
        assert!(made[1].colors.is_empty());
    }

    #[test]
    fn a_picture_of_the_page_is_measured_before_it_is_drawn() {
        let dir = db::testing::dir("viewer-sizes");
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend([0, 0, 0, 40, 0, 0, 0, 30]);
        std::fs::write(dir.join("shot.png"), png).unwrap();
        let made = rows(
            "![a](shot.png?raw=1) [![b](shot.png)](https://example.com) ![c](gone.png) ![d](https://example.com/e.png)",
            &dir,
        );
        assert_eq!(
            made[0].sizes,
            [
                ("shot.png?raw=1".to_string(), (40, 30)),
                ("shot.png".to_string(), (40, 30)),
            ]
        );
    }

    #[test]
    fn files_are_told_apart_by_how_they_end() {
        assert!(is_picture(Path::new("a/shot.PNG")));
        assert!(is_picture(Path::new("photo.jpeg")));
        assert!(!is_picture(Path::new("logo.svg")));
        assert!(!is_picture(Path::new("notes.md")));
        assert!(has_preview(Path::new("logo.svg")));
        assert!(has_preview(Path::new("README.md")));
        assert!(has_preview(Path::new("notes.MARKDOWN")));
        assert!(!has_preview(Path::new("main.rs")));
        assert_eq!(size_words(512), "512 B");
        assert_eq!(size_words(1536), "1.5 KB");
        assert_eq!(size_words(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn only_the_blocks_that_changed_are_measured_again() {
        let mut preview = Preview {
            rows: Arc::default(),
            list: ListState::new(0, ListAlignment::Top, px(600.)),
            width: 600.,
        };
        preview.put(rows("# One\n\ntwo\n\nthree\n", Path::new("")));
        assert_eq!(preview.list.item_count(), 3);
        // Scrolled to the last block, and a block before it is typed into.
        preview.list.scroll_to(gpui::ListOffset {
            item_ix: 2,
            offset_in_item: px(0.),
        });
        preview.put(rows("# One\n\ntwo and more\n\nthree\n", Path::new("")));
        assert_eq!(preview.list.item_count(), 3);
        assert_eq!(preview.list.logical_scroll_top().item_ix, 2);
        preview.put(rows(
            "# One\n\nnew\n\ntwo and more\n\nthree\n",
            Path::new(""),
        ));
        assert_eq!(preview.list.item_count(), 4);
        assert_eq!(preview.list.logical_scroll_top().item_ix, 3);
        preview.put(rows("", Path::new("")));
        assert_eq!(preview.list.item_count(), 0);
    }
}
