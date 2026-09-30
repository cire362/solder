//! Language-server features of the editor: completions, hover, go to
//! definition, references, rename, formatting and diagnostic navigation.

use std::{ops::Range, path::PathBuf, time::Duration};

use futures::future::BoxFuture;
use gpui::{
    AnyElement, App, Context, Corner, MouseButton, Pixels, Point, ScrollStrategy, SharedString,
    Task, Window, anchored, deferred, div, point, prelude::*, px, uniform_list,
};
use lsp::{Encoding, types as lt};
use text::Buffer;

use crate::{
    completion::{CompletionMenu, expand_snippet, kind_badge},
    document::Severity,
    editor::{
        ConfirmCompletion, Editor, EditorEvent, FindReferences, FormatDocument, GoToDefinition,
        HideCompletions, NextDiagnostic, PrevDiagnostic, RenameSymbol, SelectNextCompletion,
        SelectPrevCompletion, ShowCompletions, ShowHover,
    },
    lsp_store::{LspStore, from_range, to_position},
    picker::highlighted_text,
    settings::Settings,
    theme::{ActiveTheme, UI_FONT_SIZE},
};

/// How long the pointer rests before a hover request goes out.
const HOVER_DELAY: Duration = Duration::from_millis(350);
const COMPLETION_ROWS: usize = 10;
const COMPLETION_ROW_HEIGHT: Pixels = px(24.);

pub struct Hover {
    /// The hovered word; the popover goes away when the pointer leaves it.
    pub range: Range<usize>,
    pub blocks: Vec<HoverBlock>,
}

pub enum HoverBlock {
    Diagnostic(Severity, String),
    Text(String),
    Code(String),
}

/// A location returned by a language server, before its file is open.
#[derive(Clone)]
pub struct LspLocation {
    pub path: PathBuf,
    pub range: lt::Range,
    pub encoding: Encoding,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn word_start(buffer: &Buffer, offset: usize) -> usize {
    let mut start = offset;
    while let Some(c) = buffer.char_before(start) {
        if !is_word_char(c) {
            break;
        }
        start -= c.len_utf8();
    }
    start
}

fn locations(response: Option<lt::GotoDefinitionResponse>, encoding: Encoding) -> Vec<LspLocation> {
    let to = |uri: &lt::Uri, range: lt::Range| {
        lsp::uri_to_path(uri).map(|path| LspLocation {
            path,
            range,
            encoding,
        })
    };
    match response {
        Some(lt::GotoDefinitionResponse::Scalar(l)) => to(&l.uri, l.range).into_iter().collect(),
        Some(lt::GotoDefinitionResponse::Array(ls)) => {
            ls.iter().filter_map(|l| to(&l.uri, l.range)).collect()
        }
        Some(lt::GotoDefinitionResponse::Link(links)) => links
            .iter()
            .filter_map(|l| to(&l.target_uri, l.target_selection_range))
            .collect(),
        None => Vec::new(),
    }
}

/// Splits markdown into prose and fenced code blocks.
fn markdown_blocks(markdown: &str) -> Vec<HoverBlock> {
    let mut blocks = Vec::new();
    let mut in_code = false;
    let mut current = String::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            let text = std::mem::take(&mut current).trim().to_string();
            if !text.is_empty() {
                blocks.push(if in_code {
                    HoverBlock::Code(text)
                } else {
                    HoverBlock::Text(text)
                });
            }
            in_code = !in_code;
            continue;
        }
        if !in_code && line.trim() == "---" {
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    let text = current.trim().to_string();
    if !text.is_empty() {
        blocks.push(if in_code {
            HoverBlock::Code(text)
        } else {
            HoverBlock::Text(text)
        });
    }
    blocks
}

fn hover_blocks(contents: lt::HoverContents) -> Vec<HoverBlock> {
    let marked = |m: lt::MarkedString| match m {
        lt::MarkedString::String(s) => markdown_blocks(&s),
        lt::MarkedString::LanguageString(ls) => vec![HoverBlock::Code(ls.value)],
    };
    match contents {
        lt::HoverContents::Scalar(m) => marked(m),
        lt::HoverContents::Array(ms) => ms.into_iter().flat_map(marked).collect(),
        lt::HoverContents::Markup(m) => markdown_blocks(&m.value),
    }
}

impl Editor {
    fn lsp_request<R: lt::request::Request>(
        &self,
        cx: &App,
        params: impl FnOnce(lt::TextDocumentIdentifier, Encoding, &Buffer) -> R::Params,
    ) -> Option<(Encoding, BoxFuture<'static, lsp::Result<R::Result>>)> {
        let store = LspStore::global(cx)?;
        store.read(cx).request::<R>(&self.document, cx, params)
    }

    fn position_params(
        id: lt::TextDocumentIdentifier,
        encoding: Encoding,
        buffer: &Buffer,
        offset: usize,
    ) -> lt::TextDocumentPositionParams {
        lt::TextDocumentPositionParams {
            text_document: id,
            position: to_position(buffer, offset, encoding),
        }
    }

    pub(crate) fn hide_popovers(&mut self, cx: &mut Context<Self>) {
        if self.completion.is_some() || self.hover.is_some() || self.signature.is_some() {
            self.completion = None;
            self.completion_task = None;
            self.hover = None;
            self.signature = None;
            cx.notify();
        }
        self.hover_task = None;
        self.signature_task = None;
    }

    // ------------------------------------------------------------ completions

    /// Called after typed text is inserted: refilter an open menu, or open one
    /// on a word character or a server trigger character.
    pub(crate) fn after_typing(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.is_single_line() || self.selections.len() != 1 {
            return;
        }
        // Run after this effect cycle: the document's didChange for what was
        // just typed must reach the server before any request about it.
        let text = text.to_string();
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            this.update(cx, |editor, cx| editor.after_typing_synced(&text, cx))
                .ok();
        });
    }

    fn after_typing_synced(&mut self, text: &str, cx: &mut Context<Self>) {
        self.hover = None;
        let Some(c) = text.chars().last() else { return };
        self.maybe_signature_help(c, cx);
        let triggers: Vec<String> = LspStore::global(cx)
            .and_then(|s| s.read(cx).capabilities(&self.document))
            .and_then(|c| c.completion_provider)
            .and_then(|p| p.trigger_characters)
            .unwrap_or_default();
        let is_trigger = triggers.iter().any(|t| t.ends_with(c));
        if is_trigger {
            self.request_completions(Some(c), cx);
        } else if is_word_char(c) {
            if self.completion.is_some() {
                self.refilter_completions(cx);
            } else {
                self.request_completions(None, cx);
            }
        } else if self.completion.is_some() {
            self.completion = None;
            cx.notify();
        }
    }

    pub(crate) fn refilter_completions(&mut self, cx: &mut Context<Self>) {
        let head = self.newest_range().end;
        let Some(menu) = self.completion.as_mut() else {
            return;
        };
        let buffer = self.document.read(cx).text();
        if head < menu.word_start
            || buffer.offset_to_point(head).row != buffer.offset_to_point(menu.word_start).row
        {
            self.completion = None;
            cx.notify();
            return;
        }
        let query = buffer.text_for_range(menu.word_start..head);
        if !menu.filter(&query) {
            self.completion = None;
        }
        cx.notify();
    }

    pub(crate) fn show_completions(
        &mut self,
        _: &ShowCompletions,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_completions(None, cx);
    }

    fn request_completions(&mut self, trigger: Option<char>, cx: &mut Context<Self>) {
        let head = self.newest_range().end;
        let start = word_start(self.document.read(cx).text(), head);
        let Some((encoding, request)) =
            self.lsp_request::<lt::request::Completion>(cx, |id, enc, buf| lt::CompletionParams {
                text_document_position: Self::position_params(id, enc, buf, head),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: Some(lt::CompletionContext {
                    trigger_kind: if trigger.is_some() {
                        lt::CompletionTriggerKind::TRIGGER_CHARACTER
                    } else {
                        lt::CompletionTriggerKind::INVOKED
                    },
                    trigger_character: trigger.map(|c| c.to_string()),
                }),
            })
        else {
            return;
        };
        self.completion_task = Some(cx.spawn(async move |this, cx| {
            let response = request.await;
            this.update(cx, |this, cx| {
                let items = match response {
                    Ok(Some(lt::CompletionResponse::Array(items))) => items,
                    Ok(Some(lt::CompletionResponse::List(list))) => list.items,
                    _ => Vec::new(),
                };
                // The cursor may have moved while we waited.
                let head_now = this.newest_range().end;
                if items.is_empty() || head_now < start {
                    this.completion = None;
                    cx.notify();
                    return;
                }
                let mut menu = CompletionMenu::new(items, encoding, start);
                let query = this
                    .document
                    .read(cx)
                    .text()
                    .text_for_range(start..head_now);
                this.completion = menu.filter(&query).then_some(menu);
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn select_next_completion(
        &mut self,
        _: &SelectNextCompletion,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(menu) = self.completion.as_mut() {
            menu.selected = (menu.selected + 1) % menu.filtered.len();
            menu.scroll
                .scroll_to_item(menu.selected, ScrollStrategy::Top);
            cx.notify();
        }
    }

    pub(crate) fn select_prev_completion(
        &mut self,
        _: &SelectPrevCompletion,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(menu) = self.completion.as_mut() {
            let len = menu.filtered.len();
            menu.selected = (menu.selected + len - 1) % len;
            menu.scroll
                .scroll_to_item(menu.selected, ScrollStrategy::Top);
            cx.notify();
        }
    }

    pub(crate) fn hide_completions(
        &mut self,
        _: &HideCompletions,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.completion = None;
        self.completion_task = None;
        cx.notify();
    }

    pub(crate) fn confirm_completion(
        &mut self,
        _: &ConfirmCompletion,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.completion.take() else {
            return;
        };
        let Some(item) = menu.selected_item().cloned() else {
            return;
        };
        let encoding = menu.encoding;
        let head = self.newest_range().end;
        let buffer = self.document.read(cx).text();
        let (mut range, new_text) = match &item.text_edit {
            Some(lt::CompletionTextEdit::Edit(e)) => {
                (from_range(buffer, e.range, encoding), e.new_text.clone())
            }
            Some(lt::CompletionTextEdit::InsertAndReplace(e)) => {
                (from_range(buffer, e.replace, encoding), e.new_text.clone())
            }
            None => (
                menu.word_start..head,
                item.insert_text
                    .clone()
                    .unwrap_or_else(|| item.label.clone()),
            ),
        };
        // The server computed its range before the latest keystrokes.
        range.end = range.end.max(head);
        let (text, selection) = if item.insert_text_format == Some(lt::InsertTextFormat::SNIPPET) {
            expand_snippet(&new_text)
        } else {
            let len = new_text.len();
            (new_text, len..len)
        };
        let mut edits = vec![(range.clone(), text)];
        let mut shift: isize = 0;
        for extra in item.additional_text_edits.unwrap_or_default() {
            let r = from_range(buffer, extra.range, encoding);
            if r.end <= range.start {
                shift += extra.new_text.len() as isize - r.len() as isize;
            }
            edits.push((r, extra.new_text));
        }
        let start = (range.start as isize + shift) as usize;
        // The origin must be this editor, not the document, or our own
        // subscription would move the cursor a second time.
        let origin = cx.entity_id();
        self.document
            .update(cx, |d, cx| d.apply_edits(edits, Some(origin), cx));
        self.select_range(start + selection.start..start + selection.end, cx);
    }

    pub(crate) fn render_completions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.completion.as_ref()?;
        let layout = self.layout.as_ref()?;
        let bounds = layout.bounds_for_offset(
            self.document.read(cx).text(),
            self.scroll,
            menu.word_start,
        )?;
        let theme = cx.theme().clone();
        let count = menu.filtered.len();
        let height = COMPLETION_ROW_HEIGHT * count.min(COMPLETION_ROWS) as f32 + px(8.);
        let list =
            uniform_list(
                "completions",
                count,
                cx.processor(move |this, range: Range<usize>, _, cx| {
                    let theme = cx.theme().clone();
                    let Some(menu) = this.completion.as_ref() else {
                        return Vec::new();
                    };
                    range
                        .map(|ix| {
                            let (item_ix, positions) = &menu.filtered[ix];
                            let item = &menu.items[*item_ix];
                            let (badge, color) = kind_badge(item.kind, &theme);
                            let detail = item
                                .label_details
                                .as_ref()
                                .and_then(|d| d.description.clone().or(d.detail.clone()))
                                .or_else(|| item.detail.clone())
                                .unwrap_or_default();
                            div()
                                .id(ix)
                                .h(COMPLETION_ROW_HEIGHT)
                                .mx_1()
                                .px_1p5()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded(px(6.))
                                .when(ix == menu.selected, |d| d.bg(theme.accent_soft))
                                .child(div().w(px(14.)).flex_none().text_color(color).child(badge))
                                .child(div().flex_none().text_color(theme.fg).child(
                                    highlighted_text(
                                        &item.label,
                                        positions,
                                        theme.fg,
                                        theme.accent,
                                    ),
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_right()
                                        .text_size(px(11.))
                                        .text_color(theme.fg_subtle)
                                        .child(detail),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        if let Some(menu) = this.completion.as_mut() {
                                            menu.selected = ix;
                                        }
                                        this.confirm_completion(&ConfirmCompletion, window, cx);
                                    }),
                                )
                        })
                        .collect()
                }),
            )
            .track_scroll(menu.scroll.clone())
            .h(height)
            .py_1();
        let settings = Settings::get(cx);
        Some(
            deferred(
                anchored()
                    .position(point(bounds.left() - px(24.), bounds.bottom() + px(2.)))
                    .anchor(Corner::TopLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .occlude()
                            .w(px(440.))
                            .bg(theme.bg_elev)
                            .border_1()
                            .border_color(theme.line)
                            .rounded(px(8.))
                            .shadow_lg()
                            .font_family(settings.buffer_font_family.clone())
                            .text_size(px(12.5))
                            .child(list),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    // ------------------------------------------------------------ hover

    /// Pointer moved over the text with no button held.
    pub(crate) fn hover_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(offset) = self
            .layout
            .as_ref()
            .and_then(|l| l.text_offset_at(self.document.read(cx).text(), self.scroll, position))
        else {
            if self.hover.is_some() {
                self.hover = None;
                cx.notify();
            }
            self.hover_task = None;
            return;
        };
        if let Some(hover) = &self.hover {
            if hover.range.start <= offset && offset <= hover.range.end {
                return;
            }
            self.hover = None;
            cx.notify();
        }
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            this.update(cx, |this, cx| this.show_hover_at(offset, cx))
                .ok();
        }));
    }

    pub(crate) fn show_hover(&mut self, _: &ShowHover, _: &mut Window, cx: &mut Context<Self>) {
        let head = self.newest_range().end;
        self.show_hover_at(head, cx);
    }

    fn show_hover_at(&mut self, offset: usize, cx: &mut Context<Self>) {
        let doc = self.document.read(cx);
        let buffer = doc.text();
        let word = buffer.word_range_at(offset);
        let mut blocks: Vec<HoverBlock> = doc
            .diagnostics()
            .iter()
            .filter(|d| d.range.start <= offset && offset <= d.range.end.max(d.range.start + 1))
            .map(|d| {
                let text = match &d.source {
                    Some(source) => format!("{} ({source})", d.message),
                    None => d.message.clone(),
                };
                HoverBlock::Diagnostic(d.severity, text)
            })
            .collect();
        let range = if word.is_empty() {
            offset..offset
        } else {
            word
        };
        if !blocks.is_empty() {
            self.hover = Some(Hover {
                range: range.clone(),
                blocks: std::mem::take(&mut blocks),
            });
            cx.notify();
        }
        let Some((_, request)) =
            self.lsp_request::<lt::request::HoverRequest>(cx, |id, enc, buf| lt::HoverParams {
                text_document_position_params: Self::position_params(id, enc, buf, offset),
                work_done_progress_params: Default::default(),
            })
        else {
            return;
        };
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Some(hover)) = request.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let mut extra = hover_blocks(hover.contents);
                if extra.is_empty() {
                    return;
                }
                match this.hover.as_mut() {
                    Some(h) if h.range == range => h.blocks.append(&mut extra),
                    _ => {
                        this.hover = Some(Hover {
                            range,
                            blocks: extra,
                        })
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn render_hover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let hover = self.hover.as_ref()?;
        let layout = self.layout.as_ref()?;
        let bounds = layout.bounds_for_offset(
            self.document.read(cx).text(),
            self.scroll,
            hover.range.start,
        )?;
        let theme = cx.theme().clone();
        let code_font = Settings::get(cx).buffer_font_family.clone();
        let blocks = hover.blocks.iter().map(|b| match b {
            HoverBlock::Diagnostic(severity, message) => div()
                .pl_2()
                .border_l_2()
                .border_color(match severity {
                    Severity::Error => theme.error,
                    Severity::Warning => theme.warning,
                    _ => theme.accent,
                })
                .text_color(theme.fg)
                .child(message.clone()),
            HoverBlock::Code(code) => div()
                .px_2()
                .py_1p5()
                .rounded(px(6.))
                .bg(theme.bg_sunken)
                .font_family(code_font.clone())
                .text_size(px(12.))
                .text_color(theme.fg)
                .child(code.clone()),
            HoverBlock::Text(text) => div().text_color(theme.fg_muted).child(text.clone()),
        });
        Some(
            deferred(
                anchored()
                    .position(point(bounds.left(), bounds.top() - px(4.)))
                    .anchor(Corner::BottomLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("hover")
                            .occlude()
                            .max_w(px(560.))
                            .max_h(px(360.))
                            .overflow_y_scroll()
                            .p_2p5()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .bg(theme.bg_elev)
                            .border_1()
                            .border_color(theme.line)
                            .rounded(px(8.))
                            .shadow_lg()
                            .text_size(UI_FONT_SIZE)
                            .children(blocks),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    // ------------------------------------------------------------ navigation

    pub(crate) fn go_to_definition(
        &mut self,
        _: &GoToDefinition,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let head = self.newest_range().end;
        let Some((encoding, request)) =
            self.lsp_request::<lt::request::GotoDefinition>(cx, |id, enc, buf| {
                lt::GotoDefinitionParams {
                    text_document_position_params: Self::position_params(id, enc, buf, head),
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                }
            })
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let found = locations(request.await.ok().flatten(), encoding);
            this.update(cx, |_, cx| {
                cx.emit(EditorEvent::OpenLocations {
                    title: "Definitions".into(),
                    locations: found,
                    always_list: false,
                })
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn find_references(
        &mut self,
        _: &FindReferences,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let head = self.newest_range().end;
        let Some((encoding, request)) =
            self.lsp_request::<lt::request::References>(cx, |id, enc, buf| lt::ReferenceParams {
                text_document_position: Self::position_params(id, enc, buf, head),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: lt::ReferenceContext {
                    include_declaration: true,
                },
            })
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let found = locations(
                request
                    .await
                    .ok()
                    .flatten()
                    .map(lt::GotoDefinitionResponse::Array),
                encoding,
            );
            this.update(cx, |_, cx| {
                cx.emit(EditorEvent::OpenLocations {
                    title: "References".into(),
                    locations: found,
                    always_list: true,
                })
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn rename_symbol(
        &mut self,
        _: &RenameSymbol,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let head = self.newest_range().end;
        let buffer = self.document.read(cx).text();
        let word = buffer.word_range_at(head);
        if word.is_empty() {
            return;
        }
        let current = buffer.text_for_range(word);
        cx.emit(EditorEvent::RenameRequested { current });
    }

    /// Asks the server to rename the symbol at the cursor. The workspace
    /// applies the resulting edit, since it can span files.
    pub fn perform_rename(&mut self, new_name: String, cx: &mut Context<Self>) {
        let head = self.newest_range().end;
        let Some((encoding, request)) =
            self.lsp_request::<lt::request::Rename>(cx, |id, enc, buf| lt::RenameParams {
                text_document_position: Self::position_params(id, enc, buf, head),
                new_name,
                work_done_progress_params: Default::default(),
            })
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |_, cx| match result {
                Ok(Some(edit)) => cx.emit(EditorEvent::ApplyWorkspaceEdit { edit, encoding }),
                Ok(None) => {}
                Err(err) => eprintln!("rename failed: {err}"),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn format_document(
        &mut self,
        _: &FormatDocument,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.format(cx).detach();
    }

    /// Formats with the language server. Resolves once the edits are applied
    /// (or immediately when there is no server).
    pub fn format(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some((encoding, request)) =
            self.lsp_request::<lt::request::Formatting>(cx, |id, _, buf| {
                let tab = buf.line_str(0).starts_with('\t');
                lt::DocumentFormattingParams {
                    text_document: id,
                    options: lt::FormattingOptions {
                        tab_size: 4,
                        insert_spaces: !tab,
                        ..Default::default()
                    },
                    work_done_progress_params: Default::default(),
                }
            })
        else {
            return Task::ready(());
        };
        let version = self.document.read(cx).version();
        cx.spawn(async move |this, cx| {
            let Ok(Some(edits)) = request.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let doc = this.document.read(cx);
                // Formatting of stale text would scramble newer edits.
                if doc.version() != version {
                    return;
                }
                let buffer = doc.text();
                let edits = edits
                    .into_iter()
                    .map(|e| (from_range(buffer, e.range, encoding), e.new_text))
                    .collect();
                let origin = cx.entity_id();
                this.document
                    .update(cx, |d, cx| d.apply_edits(edits, Some(origin), cx));
            })
            .ok();
        })
    }

    fn jump_to_diagnostic(&mut self, forward: bool, cx: &mut Context<Self>) {
        let head = self.newest_range().start;
        let diagnostics = self.document.read(cx).diagnostics().clone();
        if diagnostics.is_empty() {
            return;
        }
        let target = if forward {
            diagnostics
                .iter()
                .find(|d| d.range.start > head)
                .or(diagnostics.first())
        } else {
            diagnostics
                .iter()
                .rev()
                .find(|d| d.range.start < head)
                .or(diagnostics.last())
        };
        if let Some(d) = target {
            let start = d.range.start;
            self.select_range(start..start, cx);
            self.show_hover_at(start, cx);
        }
    }

    pub(crate) fn next_diagnostic(
        &mut self,
        _: &NextDiagnostic,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.jump_to_diagnostic(true, cx);
    }

    pub(crate) fn prev_diagnostic(
        &mut self,
        _: &PrevDiagnostic,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.jump_to_diagnostic(false, cx);
    }
}

/// Counts for the status bar: (errors, warnings).
pub fn diagnostic_counts(doc: &crate::document::Document) -> (usize, usize) {
    doc.diagnostics()
        .iter()
        .fold((0, 0), |(e, w), d| match d.severity {
            Severity::Error => (e + 1, w),
            Severity::Warning => (e, w + 1),
            _ => (e, w),
        })
}

pub fn status_text(errors: usize, warnings: usize) -> Option<SharedString> {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    match (errors, warnings) {
        (0, 0) => None,
        (e, 0) => Some(plural(e, "error", "errors").into()),
        (0, w) => Some(plural(w, "warning", "warnings").into()),
        (e, w) => Some(
            format!(
                "{}, {}",
                plural(e, "error", "errors"),
                plural(w, "warning", "warnings")
            )
            .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_splits_code_fences() {
        let blocks = markdown_blocks("```rust\nfn a()\n```\n---\nDoes a thing.");
        assert!(matches!(&blocks[0], HoverBlock::Code(c) if c == "fn a()"));
        assert!(matches!(&blocks[1], HoverBlock::Text(t) if t == "Does a thing."));
    }

    #[test]
    fn word_start_stops_at_punctuation() {
        let b = Buffer::new("foo.bar_baz");
        assert_eq!(word_start(&b, 11), 4);
        assert_eq!(word_start(&b, 4), 4);
    }
}

/// The signature being called, with the parameter under the cursor.
pub struct SignatureHint {
    pub label: String,
    /// Byte range of the active parameter within `label`.
    pub active: Option<Range<usize>>,
    /// Where the popover is anchored (the cursor when it was requested).
    pub anchor: usize,
}

fn signature_hint(help: lt::SignatureHelp, anchor: usize) -> Option<SignatureHint> {
    let index = help.active_signature.unwrap_or(0) as usize;
    let signature = help.signatures.get(index).or(help.signatures.first())?;
    let active_param = signature
        .active_parameter
        .or(help.active_parameter)
        .unwrap_or(0) as usize;
    let active = signature
        .parameters
        .as_ref()
        .and_then(|p| p.get(active_param))
        .and_then(|p| match &p.label {
            lt::ParameterLabel::Simple(s) => {
                signature.label.find(s.as_str()).map(|i| i..i + s.len())
            }
            lt::ParameterLabel::LabelOffsets([start, end]) => {
                // Offsets are UTF-16 code units into the label.
                let to_byte = |utf16: u32| {
                    let mut units = 0;
                    for (i, c) in signature.label.char_indices() {
                        if units >= utf16 as usize {
                            return i;
                        }
                        units += c.len_utf16();
                    }
                    signature.label.len()
                };
                Some(to_byte(*start)..to_byte(*end))
            }
        });
    Some(SignatureHint {
        label: signature.label.clone(),
        active,
        anchor,
    })
}

impl Editor {
    // ------------------------------------------------------------ signature help

    pub(crate) fn maybe_signature_help(&mut self, typed: char, cx: &mut Context<Self>) {
        let caps = LspStore::global(cx)
            .and_then(|s| s.read(cx).capabilities(&self.document))
            .and_then(|c| c.signature_help_provider);
        let Some(caps) = caps else { return };
        let triggers = caps.trigger_characters.unwrap_or_default();
        let retriggers = caps.retrigger_characters.unwrap_or_default();
        let matches = |list: &[String]| list.iter().any(|t| t.ends_with(typed));
        if typed == ')' {
            self.signature = None;
            cx.notify();
        } else if matches(&triggers) || (self.signature.is_some() && matches(&retriggers)) {
            self.request_signature_help(Some(typed), cx);
        }
    }

    fn request_signature_help(&mut self, trigger: Option<char>, cx: &mut Context<Self>) {
        let head = self.newest_range().end;
        let retrigger = self.signature.is_some();
        let Some((_, request)) =
            self.lsp_request::<lt::request::SignatureHelpRequest>(cx, |id, enc, buf| {
                lt::SignatureHelpParams {
                    context: Some(lt::SignatureHelpContext {
                        trigger_kind: if trigger.is_some() {
                            lt::SignatureHelpTriggerKind::TRIGGER_CHARACTER
                        } else {
                            lt::SignatureHelpTriggerKind::INVOKED
                        },
                        trigger_character: trigger.map(|c| c.to_string()),
                        is_retrigger: retrigger,
                        active_signature_help: None,
                    }),
                    text_document_position_params: Self::position_params(id, enc, buf, head),
                    work_done_progress_params: Default::default(),
                }
            })
        else {
            return;
        };
        self.signature_task = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |this, cx| {
                this.signature = result
                    .ok()
                    .flatten()
                    .and_then(|help| signature_hint(help, head));
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn render_signature(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let hint = self.signature.as_ref()?;
        // Completions take the space below the cursor; the signature sits above.
        let layout = self.layout.as_ref()?;
        let bounds =
            layout.bounds_for_offset(self.document.read(cx).text(), self.scroll, hint.anchor)?;
        let theme = cx.theme().clone();
        let settings = Settings::get(cx);
        let highlights = hint
            .active
            .clone()
            .map(|r| {
                vec![(
                    r,
                    gpui::HighlightStyle {
                        color: Some(theme.accent),
                        font_weight: Some(gpui::FontWeight::BOLD),
                        ..Default::default()
                    },
                )]
            })
            .unwrap_or_default();
        Some(
            deferred(
                anchored()
                    .position(point(bounds.left(), bounds.top() - px(4.)))
                    .anchor(Corner::BottomLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .occlude()
                            .max_w(px(640.))
                            .px_2()
                            .py_1()
                            .bg(theme.bg_elev)
                            .border_1()
                            .border_color(theme.line)
                            .rounded(px(8.))
                            .shadow_lg()
                            .font_family(settings.buffer_font_family.clone())
                            .text_size(px(12.))
                            .text_color(theme.fg_muted)
                            .child(
                                gpui::StyledText::new(SharedString::from(hint.label.clone()))
                                    .with_highlights(highlights),
                            ),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    // ------------------------------------------------------------ code actions

    pub(crate) fn code_actions(
        &mut self,
        _: &crate::editor::CodeActions,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.newest_range();
        let doc = self.document.read(cx);
        let diagnostics: Vec<(Range<usize>, Severity, String)> = doc
            .diagnostics()
            .iter()
            .filter(|d| d.range.start <= range.end && d.range.end >= range.start)
            .map(|d| (d.range.clone(), d.severity, d.message.clone()))
            .collect();
        let Some((encoding, request)) =
            self.lsp_request::<lt::request::CodeActionRequest>(cx, |id, enc, buf| {
                let to_range = |r: Range<usize>| {
                    lt::Range::new(to_position(buf, r.start, enc), to_position(buf, r.end, enc))
                };
                lt::CodeActionParams {
                    text_document: id,
                    range: to_range(range.clone()),
                    context: lt::CodeActionContext {
                        diagnostics: diagnostics
                            .into_iter()
                            .map(|(r, severity, message)| lt::Diagnostic {
                                range: to_range(r),
                                severity: Some(match severity {
                                    Severity::Error => lt::DiagnosticSeverity::ERROR,
                                    Severity::Warning => lt::DiagnosticSeverity::WARNING,
                                    Severity::Info => lt::DiagnosticSeverity::INFORMATION,
                                    Severity::Hint => lt::DiagnosticSeverity::HINT,
                                }),
                                message,
                                ..Default::default()
                            })
                            .collect(),
                        only: None,
                        trigger_kind: Some(lt::CodeActionTriggerKind::INVOKED),
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                }
            })
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let actions = request.await.ok().flatten().unwrap_or_default();
            this.update(cx, |_, cx| {
                cx.emit(EditorEvent::ShowCodeActions { actions, encoding })
            })
            .ok();
        })
        .detach();
    }

    /// Runs a chosen code action: its edit, resolved first if the server
    /// sent it without one, then its command.
    pub fn apply_code_action(
        &mut self,
        action: lt::CodeActionOrCommand,
        encoding: Encoding,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = LspStore::global(cx) else {
            return;
        };
        let document = self.document.clone();
        match action {
            lt::CodeActionOrCommand::Command(command) => {
                let request = store
                    .read(cx)
                    .server_request::<lt::request::ExecuteCommand>(
                        &document,
                        lt::ExecuteCommandParams {
                            command: command.command,
                            arguments: command.arguments.unwrap_or_default(),
                            work_done_progress_params: Default::default(),
                        },
                    );
                if let Some(request) = request {
                    cx.background_executor()
                        .spawn(async move {
                            let _ = request.await;
                        })
                        .detach();
                }
            }
            lt::CodeActionOrCommand::CodeAction(action) => {
                let resolve = (action.edit.is_none() && action.data.is_some())
                    .then(|| {
                        store
                            .read(cx)
                            .server_request::<lt::request::CodeActionResolveRequest>(
                                &document,
                                action.clone(),
                            )
                    })
                    .flatten();
                cx.spawn(async move |this, cx| {
                    let action = match resolve {
                        Some(request) => request.await.unwrap_or(action),
                        None => action,
                    };
                    this.update(cx, |this, cx| {
                        if let Some(edit) = action.edit {
                            cx.emit(EditorEvent::ApplyWorkspaceEdit { edit, encoding });
                        }
                        if let Some(command) = action.command {
                            this.apply_code_action(
                                lt::CodeActionOrCommand::Command(command),
                                encoding,
                                cx,
                            );
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }
}

#[cfg(test)]
mod signature_tests {
    use super::*;

    #[test]
    fn active_parameter_from_offsets_or_text() {
        let help = lt::SignatureHelp {
            signatures: vec![lt::SignatureInformation {
                label: "fn add(a: i32, b: i32) -> i32".into(),
                documentation: None,
                parameters: Some(vec![
                    lt::ParameterInformation {
                        label: lt::ParameterLabel::LabelOffsets([7, 13]),
                        documentation: None,
                    },
                    lt::ParameterInformation {
                        label: lt::ParameterLabel::Simple("b: i32".into()),
                        documentation: None,
                    },
                ]),
                active_parameter: None,
            }],
            active_signature: Some(0),
            active_parameter: Some(1),
        };
        let hint = signature_hint(help.clone(), 0).unwrap();
        assert_eq!(&hint.label[hint.active.unwrap()], "b: i32");
        let first = signature_hint(
            lt::SignatureHelp {
                active_parameter: Some(0),
                ..help
            },
            0,
        )
        .unwrap();
        assert_eq!(&first.label[first.active.unwrap()], "a: i32");
    }
}
