//! The ERD of a connection: tables as boxes, foreign keys as lines, on a
//! canvas that pans and zooms. Tables can be moved (the layout is saved in
//! the project), searched, opened as data or structure, joined along a
//! relation, and linked by dragging from one column to another, which
//! drafts a foreign key in the structure form. Exports as SVG, PNG and
//! Mermaid.

use std::{path::PathBuf, sync::Arc};

use db::{
    Engine, Object, Schema,
    ddl::ForeignKeyDraft,
    erd::{self, Diagram, Layout},
};
use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point,
    ScrollWheelEvent, SharedString, Task, Window, actions, canvas, div, point, prelude::*, px,
};

use crate::{
    database::{DatabaseEvent, DatabaseStore},
    editor::Editor,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(erd, [CloseDiagram, FitDiagram, ZoomIn, ZoomOut, FindTable]);

pub fn bind_keys(cx: &mut App) {
    let context = Some("ErdView");
    cx.bind_keys([
        KeyBinding::new("escape", CloseDiagram, context),
        KeyBinding::new("secondary-0", FitDiagram, context),
        KeyBinding::new("secondary-=", ZoomIn, context),
        KeyBinding::new("secondary--", ZoomOut, context),
        KeyBinding::new("enter", FindTable, Some("ErdSearch")),
    ]);
}

pub enum ErdEvent {
    Close,
    /// Browse a table's rows in Results.
    Browse(Object),
    /// Edit a table's structure; `None` for a new table.
    Structure(Option<Object>),
    /// Open a table's structure with a new foreign key drafted.
    Link(Object, ForeignKeyDraft),
    RunQuery(String),
}

impl EventEmitter<ErdEvent> for ErdView {}

enum Drag {
    Pan {
        start: Point<Pixels>,
        pan: (f32, f32),
    },
    Move {
        node: usize,
        grab: (f32, f32),
    },
    /// From a column to wherever the mouse is, in diagram coordinates.
    Link {
        node: usize,
        field: String,
        to: (f32, f32),
    },
}

/// Where a column's link handle is: the last pixels of its row.
const HANDLE: f32 = 14.;

pub struct ErdView {
    store: Entity<DatabaseStore>,
    root: PathBuf,
    pub connection: SharedString,
    engine: Engine,
    schema: Option<Arc<Schema>>,
    pub diagram: Diagram,
    /// Diagram coordinates at the canvas's top left.
    pan: (f32, f32),
    zoom: f32,
    pub selected: Option<usize>,
    search: Entity<Editor>,
    drag: Option<Drag>,
    canvas: Bounds<Pixels>,
    fit_pending: bool,
    pub notice: Option<SharedString>,
    save: Option<Task<()>>,
    focus: FocusHandle,
}

impl ErdView {
    pub fn new(
        store: Entity<DatabaseStore>,
        root: PathBuf,
        connection: SharedString,
        engine: Engine,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.subscribe(&store, |this, _, _: &DatabaseEvent, cx| this.reload(cx))
            .detach();
        let name = connection.clone();
        store.update(cx, |s, cx| s.ensure_schema(&name, cx));
        let mut view = Self {
            store,
            root,
            connection,
            engine,
            schema: None,
            diagram: Diagram::default(),
            pan: (0., 0.),
            zoom: 1.,
            selected: None,
            search: cx.new(|cx| Editor::single_line("Find a table", cx)),
            drag: None,
            canvas: Bounds::default(),
            fit_pending: true,
            notice: None,
            save: None,
            focus: cx.focus_handle(),
        };
        view.reload(cx);
        view
    }

    /// Rebuilds from the schema when it changed, keeping positions: those
    /// on screen, else those saved in the project.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some((_, schema)) = self.store.read(cx).schema_of(&self.connection) else {
            return;
        };
        if self
            .schema
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(s, &schema))
        {
            return;
        }
        let saved = if self.diagram.nodes.is_empty() {
            Layout::load(&erd::layout_path(&self.root, &self.connection))
        } else {
            Layout::of(&self.diagram)
        };
        let selected = self.selected.map(|i| self.diagram.nodes[i].id());
        self.diagram = erd::diagram(&schema, &saved);
        self.selected =
            selected.and_then(|id| self.diagram.nodes.iter().position(|n| n.id() == id));
        self.schema = Some(schema);
        cx.notify();
    }

    fn object(&self, node: usize) -> Option<Object> {
        let n = self.diagram.nodes.get(node)?;
        self.schema
            .as_ref()?
            .objects
            .iter()
            .find(|o| o.name == n.name && o.namespace == n.namespace)
            .cloned()
    }

    /// Where a diagram point is drawn in the window.
    #[cfg(test)]
    pub fn to_window(&self, (x, y): (f32, f32)) -> Point<Pixels> {
        point(
            self.canvas.origin.x + px((x - self.pan.0) * self.zoom),
            self.canvas.origin.y + px((y - self.pan.1) * self.zoom),
        )
    }

    fn to_diagram(&self, p: Point<Pixels>) -> (f32, f32) {
        (
            f32::from(p.x - self.canvas.origin.x) / self.zoom + self.pan.0,
            f32::from(p.y - self.canvas.origin.y) / self.zoom + self.pan.1,
        )
    }

    fn node_at(&self, (x, y): (f32, f32)) -> Option<usize> {
        // The last drawn is on top.
        self.diagram.nodes.iter().rposition(|n| n.contains(x, y))
    }

    /// Everything in view, not larger than life.
    pub fn fit(&mut self, cx: &mut Context<Self>) {
        let (x0, y0, x1, y1) = erd::bounds(&self.diagram);
        let (w, h) = (
            f32::from(self.canvas.size.width),
            f32::from(self.canvas.size.height),
        );
        if w <= 0. || h <= 0. {
            self.fit_pending = true;
            return;
        }
        self.zoom = ((w - 40.) / (x1 - x0))
            .min((h - 40.) / (y1 - y0))
            .clamp(0.2, 1.);
        self.pan = (x0 - 20. / self.zoom, y0 - 20. / self.zoom);
        self.fit_pending = false;
        cx.notify();
    }

    fn zoom_by(&mut self, factor: f32, around: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let anchor = around.unwrap_or(self.canvas.center());
        let before = self.to_diagram(anchor);
        self.zoom = (self.zoom * factor).clamp(0.2, 3.);
        let after = self.to_diagram(anchor);
        self.pan = (
            self.pan.0 + before.0 - after.0,
            self.pan.1 + before.1 - after.1,
        );
        cx.notify();
    }

    fn center_on(&mut self, node: usize, cx: &mut Context<Self>) {
        let n = &self.diagram.nodes[node];
        let (w, h) = (
            f32::from(self.canvas.size.width) / self.zoom,
            f32::from(self.canvas.size.height) / self.zoom,
        );
        self.pan = (n.x + n.width / 2. - w / 2., n.y + n.height() / 2. - h / 2.);
        self.selected = Some(node);
        cx.notify();
    }

    fn find_table(&mut self, _: &FindTable, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search.read(cx).text(cx).to_lowercase();
        if let Some(i) = self
            .diagram
            .nodes
            .iter()
            .position(|n| !query.is_empty() && n.name.to_lowercase().contains(&query))
        {
            self.center_on(i, cx);
            window.focus(&self.focus);
        }
    }

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let at = self.to_diagram(e.position);
        let Some(i) = self.node_at(at) else {
            self.selected = None;
            self.drag = Some(Drag::Pan {
                start: e.position,
                pan: self.pan,
            });
            return cx.notify();
        };
        self.selected = Some(i);
        let n = &self.diagram.nodes[i];
        if e.click_count == 2 {
            if let Some(object) = self.object(i) {
                cx.emit(ErdEvent::Browse(object));
            }
            return cx.notify();
        }
        let field = n.field_at(at.1).map(|f| f.name.clone());
        self.drag = match field {
            Some(field) if at.0 > n.x + n.width - HANDLE && !n.view => Some(Drag::Link {
                node: i,
                field,
                to: at,
            }),
            _ => Some(Drag::Move {
                node: i,
                grab: (at.0 - n.x, at.1 - n.y),
            }),
        };
        cx.notify();
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let at = self.to_diagram(e.position);
        match &mut self.drag {
            Some(Drag::Pan { start, pan }) => {
                self.pan = (
                    pan.0 - f32::from(e.position.x - start.x) / self.zoom,
                    pan.1 - f32::from(e.position.y - start.y) / self.zoom,
                );
            }
            Some(Drag::Move { node, grab }) => {
                let n = &mut self.diagram.nodes[*node];
                n.x = at.0 - grab.0;
                n.y = at.1 - grab.1;
            }
            Some(Drag::Link { to, .. }) => *to = at,
            None => return,
        }
        cx.notify();
    }

    fn mouse_up(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self.drag.take() {
            Some(Drag::Move { .. }) => self.save_layout(cx),
            Some(Drag::Link { node, field, .. }) => {
                let at = self.to_diagram(e.position);
                self.link(node, &field, at, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Dropping a column on a column of another table drafts a foreign key
    /// from the first to the second.
    pub fn link(&mut self, node: usize, field: &str, at: (f32, f32), cx: &mut Context<Self>) {
        let Some(target) = self.node_at(at) else {
            return;
        };
        let Some(target_field) = self.diagram.nodes[target]
            .field_at(at.1)
            .map(|f| f.name.clone())
        else {
            return;
        };
        if target == node && target_field == field {
            return;
        }
        let (from, to) = (&self.diagram.nodes[node], &self.diagram.nodes[target]);
        let fk = ForeignKeyDraft {
            original: None,
            name: format!("{}_{}_fkey", from.name, field),
            columns: vec![field.to_string()],
            ref_table: to.name.clone(),
            ref_columns: vec![target_field],
            on_delete: None,
        };
        if let Some(object) = self.object(node) {
            cx.emit(ErdEvent::Link(object, fk));
        }
    }

    fn save_layout(&mut self, cx: &mut Context<Self>) {
        let layout = Layout::of(&self.diagram);
        let path = erd::layout_path(&self.root, &self.connection);
        let write = cx
            .background_executor()
            .spawn(async move { layout.save(&path) });
        self.save = Some(cx.spawn(async move |this, cx| {
            if let Err(e) = write.await {
                this.update(cx, |this, cx| {
                    this.notice = Some(format!("Could not save the layout: {e}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn scroll(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = e.delta.pixel_delta(px(20.));
        if e.modifiers.platform || e.modifiers.control {
            let factor = (1. + f32::from(delta.y) / 300.).clamp(0.5, 2.);
            self.zoom_by(factor, Some(e.position), cx);
        } else {
            self.pan = (
                self.pan.0 - f32::from(delta.x) / self.zoom,
                self.pan.1 - f32::from(delta.y) / self.zoom,
            );
            cx.notify();
        }
    }

    pub fn export_svg(&self, path: PathBuf, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        let svg = erd::svg(&self.diagram, &erd::Palette::default());
        cx.background_executor()
            .spawn(async move { std::fs::write(path, svg).map_err(|e| e.to_string()) })
    }

    pub fn export_png(&self, path: PathBuf, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        let svg = erd::svg(&self.diagram, &erd::Palette::default());
        cx.background_executor().spawn(async move {
            // Twice the size, for sharp text on high-density screens.
            let png = erd::png(&svg, 2.)?;
            std::fs::write(path, png).map_err(|e| e.to_string())
        })
    }

    fn export(&mut self, extension: &'static str, cx: &mut Context<Self>) {
        let name = format!(
            "{}.{extension}",
            erd::layout_path(&self.root, &self.connection)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "diagram".into())
        );
        let chosen = cx.prompt_for_new_path(&self.root, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let Ok(write) = this.update(cx, |this, cx| match extension {
                "png" => this.export_png(path.clone(), cx),
                _ => this.export_svg(path.clone(), cx),
            }) else {
                return;
            };
            let result = write.await;
            this.update(cx, |this, cx| {
                this.notice = Some(match result {
                    Ok(()) => format!("Saved {}", path.display()).into(),
                    Err(e) => e.into(),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    #[cfg(test)]
    pub fn search_field(&self) -> Entity<Editor> {
        self.search.clone()
    }

    pub fn copy_mermaid(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(erd::mermaid(&self.diagram)));
        self.notice = Some("Copied the diagram as Mermaid".into());
        cx.notify();
    }

    fn render_toolbar(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        let act = |id: &'static str,
                   label: &'static str,
                   primary: bool,
                   f: fn(&mut Self, &mut Context<Self>)| {
            let view = view.clone();
            ui::button(id, label, primary, theme, move |_, _, cx| {
                view.update(cx, f)
            })
        };
        let focused = self.search.focus_handle(cx).is_focused(window);
        div()
            .flex_none()
            .h(px(48.))
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(theme.line)
            .child(
                div()
                    .flex_none()
                    .px_1p5()
                    .rounded(px(6.))
                    .bg(theme.bg_elev)
                    .text_color(theme.fg_muted)
                    .child(self.connection.clone()),
            )
            .child(
                div()
                    .key_context("ErdSearch")
                    .w(px(220.))
                    .flex()
                    .child(ui::text_field(self.search.clone(), focused, theme)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.fg_subtle)
                    .children(self.notice.clone()),
            )
            .child(act("erd-zoom-out", "−", false, |this, cx| {
                this.zoom_by(0.8, None, cx)
            }))
            .child(act("erd-zoom-in", "+", false, |this, cx| {
                this.zoom_by(1.25, None, cx)
            }))
            .child(act("erd-fit", "Fit", false, Self::fit))
            .child(act("erd-mermaid", "Mermaid", false, Self::copy_mermaid))
            .child(act("erd-svg", "SVG", false, |this, cx| {
                this.export("svg", cx)
            }))
            .child(act("erd-png", "PNG", false, |this, cx| {
                this.export("png", cx)
            }))
            .child(act("erd-new-table", "New table", true, |_, cx| {
                cx.emit(ErdEvent::Structure(None))
            }))
    }

    /// The selected table's actions and relations.
    fn render_details(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let i = self.selected?;
        let n = self.diagram.nodes.get(i)?;
        let object = self.object(i)?;
        let view = cx.entity();
        let relations: Vec<(String, String)> = self
            .diagram
            .edges
            .iter()
            .filter(|e| e.from == i || e.to == i)
            .map(|e| {
                (
                    format!(
                        "{}.{} → {}.{}",
                        self.diagram.nodes[e.from].name,
                        e.from_columns.join(", "),
                        self.diagram.nodes[e.to].name,
                        e.to_columns.join(", ")
                    ),
                    erd::join_query(self.engine, &self.diagram, e),
                )
            })
            .collect();
        Some(
            div()
                .flex_none()
                .max_h(px(160.))
                .px_4()
                .py_2()
                .flex()
                .flex_col()
                .gap_1p5()
                .border_t_1()
                .border_color(theme.line)
                .bg(theme.bg_sunken)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_color(theme.fg).child(n.name.clone()))
                        .child(ui::button("erd-data", "Data", false, theme, {
                            let (view, object) = (view.clone(), object.clone());
                            move |_, _, cx| view.update(cx, |_, cx| cx.emit(ErdEvent::Browse(object.clone())))
                        }))
                        .when(!n.view, |d| {
                            d.child(ui::button("erd-structure", "Structure", false, theme, {
                                let (view, object) = (view.clone(), object.clone());
                                move |_, _, cx| {
                                    view.update(cx, |_, cx| cx.emit(ErdEvent::Structure(Some(object.clone()))))
                                }
                            }))
                        })
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.fg_subtle)
                                .child("Drag from a column's right edge onto another column to add a foreign key"),
                        ),
                )
                .children(relations.into_iter().enumerate().map(|(k, (label, query))| {
                    let view = view.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(11.5))
                        .child(div().font_family(crate::theme::CODE_FONT).text_color(theme.fg_muted).child(label))
                        .child(ui::button(("erd-join", k), "Join", false, theme, move |_, _, cx| {
                            view.update(cx, |_, cx| cx.emit(ErdEvent::RunQuery(query.clone())))
                        }))
                })),
        )
    }

    fn render_nodes(&self, theme: &Theme, query: &str) -> Vec<gpui::AnyElement> {
        let (w, h) = (
            f32::from(self.canvas.size.width) / self.zoom,
            f32::from(self.canvas.size.height) / self.zoom,
        );
        let query = query.to_lowercase();
        let z = self.zoom;
        let related: Vec<usize> = self
            .selected
            .map(|s| {
                self.diagram
                    .edges
                    .iter()
                    .filter_map(|e| match (e.from == s, e.to == s) {
                        (true, _) => Some(e.to),
                        (_, true) => Some(e.from),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.diagram
            .nodes
            .iter()
            .enumerate()
            // Only what is on screen.
            .filter(|(_, n)| {
                n.x + n.width >= self.pan.0
                    && n.x <= self.pan.0 + w
                    && n.y + n.height() >= self.pan.1
                    && n.y <= self.pan.1 + h
            })
            .map(|(i, n)| {
                let selected = self.selected == Some(i);
                let found = !query.is_empty() && n.name.to_lowercase().contains(&query);
                let border = if selected || found {
                    theme.accent
                } else if related.contains(&i) {
                    theme.fg_subtle
                } else {
                    theme.line
                };
                div()
                    .absolute()
                    .left(px((n.x - self.pan.0) * z))
                    .top(px((n.y - self.pan.1) * z))
                    .w(px(n.width * z))
                    .h(px(n.height() * z))
                    .rounded(px(8. * z))
                    .border_1()
                    .border_color(border)
                    .bg(theme.bg_elev)
                    .overflow_hidden()
                    .text_size(px(12. * z))
                    .font_family(crate::theme::CODE_FONT)
                    .child(
                        div()
                            .h(px(erd::HEADER * z))
                            .px(px(12. * z))
                            .flex()
                            .items_center()
                            .gap(px(6. * z))
                            .bg(theme.bg_sunken)
                            .text_color(theme.fg)
                            .child(n.name.clone())
                            .when(n.view, |d| {
                                d.child(div().text_color(theme.fg_subtle).child("view"))
                            }),
                    )
                    .children(n.fields.iter().map(|f| {
                        div()
                            .h(px(erd::ROW * z))
                            .px(px(12. * z))
                            .flex()
                            .items_center()
                            .gap(px(6. * z))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(if f.key { theme.accent } else { theme.fg })
                                    .child(f.name.clone()),
                            )
                            .when(f.reference, |d| {
                                d.child(div().text_color(theme.accent).child("→"))
                            })
                            .child(div().flex_1())
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.fg_subtle)
                                    .child(f.type_name.clone()),
                            )
                            .child(
                                // The link handle.
                                div()
                                    .flex_none()
                                    .size(px(6. * z))
                                    .rounded(px(3. * z))
                                    .bg(theme.line),
                            )
                    }))
                    .into_any_element()
            })
            .collect()
    }
}

impl Focusable for ErdView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ErdView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if self.fit_pending && !self.diagram.nodes.is_empty() && self.canvas.size.width > px(0.) {
            self.fit(cx);
        }
        // Lines in diagram coordinates, drawn by the canvas below the boxes.
        let selected = self.selected;
        let mut lines: Vec<(Vec<(f32, f32)>, bool)> = self
            .diagram
            .edges
            .iter()
            .map(|e| {
                let lit = selected.is_some_and(|s| e.from == s || e.to == s);
                (erd::route(&self.diagram, e), lit)
            })
            .collect();
        if let Some(Drag::Link { node, field, to }) = &self.drag {
            let n = &self.diagram.nodes[*node];
            lines.push((vec![(n.x + n.width, n.row_y(field)), *to], true));
        }
        let (pan, zoom) = (self.pan, self.zoom);
        let (muted, accent) = (theme.fg_subtle, theme.accent);
        let view = cx.entity();
        let empty = self.diagram.nodes.is_empty();
        div()
            .id("erd")
            .key_context("ErdView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &CloseDiagram, _, cx| cx.emit(ErdEvent::Close)))
            .on_action(cx.listener(|this, _: &FitDiagram, _, cx| this.fit(cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_by(1.25, None, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_by(0.8, None, cx)))
            .on_action(cx.listener(Self::find_table))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.fg)
            .text_size(UI_FONT_SIZE)
            .child(self.render_toolbar(&theme, window, cx))
            .child(
                div()
                    .id("erd-canvas")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_scroll_wheel(cx.listener(Self::scroll))
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                view.update(cx, |v, cx| {
                                    if v.canvas != bounds {
                                        v.canvas = bounds;
                                        cx.notify();
                                    }
                                })
                            },
                            move |bounds, _, window, _| {
                                let at = |(x, y): (f32, f32)| {
                                    point(
                                        bounds.origin.x + px((x - pan.0) * zoom),
                                        bounds.origin.y + px((y - pan.1) * zoom),
                                    )
                                };
                                for (points, lit) in &lines {
                                    let mut path = PathBuilder::stroke(px((1.5 * zoom).max(1.)));
                                    path.move_to(at(points[0]));
                                    for p in &points[1..] {
                                        path.line_to(at(*p));
                                    }
                                    if let Ok(path) = path.build() {
                                        window.paint_path(path, if *lit { accent } else { muted });
                                    }
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .children(self.render_nodes(&theme, &self.search.read(cx).text(cx)))
                    .when(empty, |d| {
                        d.child(
                            div()
                                .p_4()
                                .text_color(theme.fg_subtle)
                                .child("Reading the schema..."),
                        )
                    }),
            )
            .children(self.render_details(&theme, cx))
    }
}
