//! Native trees shared by extension views, source control and test controllers.
//! Only open branches are asked for; an older refresh cannot replace a newer one.

use crate::{
    extension_api::ExtensionEvent,
    extension_store::ExtensionStore,
    theme::{ActiveTheme, UI_FONT_SIZE},
    ui,
};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, KeyBinding, ScrollStrategy, Subscription,
    UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

type Key = (String, String);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    pub title: String,
    pub kind: String,
    pub message: Option<String>,
    pub revision: usize,
}

impl ExtensionStore {
    pub fn views(&self) -> BTreeMap<Key, View> {
        let mut views = BTreeMap::new();
        for extension in &self.installed {
            if !self.may_run(extension) {
                continue;
            }
            for (id, title) in &extension.views {
                views.insert(
                    (extension.id.clone(), id.clone()),
                    View {
                        title: title.clone(),
                        // One drawn as a page opens in a tab.
                        kind: match extension.page_views.contains(id) {
                            true => "webview".into(),
                            false => "tree".into(),
                        },
                        ..Default::default()
                    },
                );
            }
        }
        views.extend(self.api.views.clone());
        views
    }

    pub(crate) fn view_said(
        &mut self,
        owner: &str,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = params["id"].as_str() else {
            return;
        };
        let key = (owner.to_string(), id.to_string());
        if params["gone"] == true {
            self.api.views.remove(&key);
        } else {
            let view = self.api.views.entry(key).or_default();
            view.revision += 1;
            if method == "view" {
                view.title = params["title"].as_str().unwrap_or(id).to_string();
                view.kind = params["kind"].as_str().unwrap_or("tree").to_string();
                view.message = params["message"].as_str().map(str::to_string);
            }
        }
        cx.emit(ExtensionEvent::Views);
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Node {
    key: String,
    label: String,
    description: Option<String>,
    tooltip: Option<String>,
    mark: Option<String>,
    opens: bool,
    strike: bool,
    open: bool,
    run: Option<String>,
    actions: Vec<NodeAction>,
}
#[derive(Clone, Debug, Deserialize)]
struct NodeAction {
    title: String,
    run: String,
}
#[derive(Clone)]
enum Row {
    View(Key, View),
    Node(Node, usize),
    Note(String),
}

actions!(
    extension_views,
    [Next, Previous, Open, Expand, Collapse, Refresh, Run]
);
pub fn bind_keys(cx: &mut App) {
    let context = Some("ExtensionViews");
    cx.bind_keys([
        KeyBinding::new("down", Next, context),
        KeyBinding::new("up", Previous, context),
        KeyBinding::new("enter", Open, context),
        KeyBinding::new("secondary-enter", Run, context),
        KeyBinding::new("right", Expand, context),
        KeyBinding::new("left", Collapse, context),
        KeyBinding::new("secondary-r", Refresh, context),
    ]);
}

pub struct ExtensionViews {
    store: Entity<ExtensionStore>,
    views: BTreeMap<Key, View>,
    active: Option<Key>,
    visible: bool,
    children: HashMap<Option<String>, Vec<Node>>,
    opened: HashSet<String>,
    closed: HashSet<String>,
    loading: HashSet<Option<String>>,
    error: Option<String>,
    generation: usize,
    rows: Vec<Row>,
    selected: usize,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _watch: Subscription,
    _events: Subscription,
}

impl ExtensionViews {
    pub fn new(store: Entity<ExtensionStore>, cx: &mut Context<Self>) -> Self {
        let watch = cx.observe(&store, |this, _, cx| this.sync(cx));
        let events = cx.subscribe(&store, |this, _, event, cx| {
            if matches!(event, ExtensionEvent::Views) {
                this.sync(cx);
            }
        });
        let mut this = Self {
            views: store.read(cx).views(),
            store,
            active: None,
            visible: false,
            children: HashMap::new(),
            opened: HashSet::new(),
            closed: HashSet::new(),
            loading: HashSet::new(),
            error: None,
            generation: 0,
            rows: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _watch: watch,
            _events: events,
        };
        this.rebuild();
        this
    }

    #[cfg(test)]
    pub fn index(&self, label: &str) -> Option<usize> {
        self.rows.iter().position(|row| match row {
            Row::View(_, view) => view.title == label,
            Row::Node(node, _) => node.label == label,
            _ => false,
        })
    }
    #[cfg(test)]
    pub fn has(&self, label: &str) -> bool {
        self.index(label).is_some()
    }
    #[cfg(test)]
    pub fn has_mark(&self, mark: &str) -> bool {
        self.rows
            .iter()
            .any(|row| matches!(row, Row::Node(node, _) if node.mark.as_deref() == Some(mark)))
    }
    #[cfg(test)]
    pub fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        self.click(index, cx);
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible != self.visible {
            self.visible = visible;
            if let Some((_, id)) = &self.active {
                self.request("view.visible", json!({"id": id, "visible": visible}), cx);
            }
        }
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let now = self.store.read(cx).views();
        if now == self.views {
            return;
        }
        let changed = self
            .active
            .as_ref()
            .is_some_and(|key| now.get(key) != self.views.get(key));
        self.views = now;
        if self
            .active
            .as_ref()
            .is_some_and(|key| !self.views.contains_key(key))
        {
            self.active = None;
        }
        if changed {
            self.refresh(cx);
        }
        self.rebuild();
        cx.notify();
    }

    fn request(&mut self, method: &'static str, params: Value, cx: &mut Context<Self>) {
        let Some((owner, _)) = &self.active else {
            return;
        };
        let task = self
            .store
            .update(cx, |store, cx| store.ask_host(owner, method, params, cx));
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    if generation == this.generation {
                        this.error = Some(error);
                        this.rebuild();
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn choose(&mut self, key: Key, cx: &mut Context<Self>) {
        if let Some((_, id)) = &self.active {
            self.request("view.visible", json!({"id": id, "visible": false}), cx);
        }
        self.active = Some(key.clone());
        self.opened.clear();
        self.closed.clear();
        self.refresh(cx);
        self.request(
            "view.visible",
            json!({"id": key.1, "visible": self.visible}),
            cx,
        );
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.children.clear();
        self.loading.clear();
        self.error = None;
        if self.active.is_some() {
            self.fetch(None, cx);
        }
        self.rebuild();
        cx.notify();
    }

    fn fetch(&mut self, parent: Option<String>, cx: &mut Context<Self>) {
        let Some((owner, id)) = self.active.clone() else {
            return;
        };
        if self.loading.contains(&parent) || self.children.contains_key(&parent) {
            return;
        }
        // A view that is a page has no nodes: the extension is asked to
        // put it in a tab, and its code is started for that.
        let page = self.views.get(&(owner.clone(), id.clone()));
        if page.is_some_and(|view| view.kind == "webview") {
            self.children.insert(None, Vec::new());
            self.request("webview.resolve", json!({ "id": id }), cx);
            return;
        }
        self.loading.insert(parent.clone());
        let generation = self.generation;
        let task = self.store.update(cx, |store, cx| {
            store.ask_host(
                &owner,
                "view.children",
                json!({"id": id, "node": parent}),
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let answer = task.await.and_then(|value| {
                serde_json::from_value::<Vec<Node>>(value).map_err(|error| error.to_string())
            });
            this.update(cx, |this, cx| {
                if generation != this.generation {
                    return;
                }
                this.loading.remove(&parent);
                match answer {
                    Ok(mut nodes) => {
                        nodes.truncate(10000);
                        let mut seen = HashSet::new();
                        nodes.retain(|node| !node.key.is_empty() && seen.insert(node.key.clone()));
                        let expanded: Vec<_> = nodes
                            .iter()
                            .filter(|node| {
                                node.opens
                                    && (this.opened.contains(&node.key)
                                        || (node.open && !this.closed.contains(&node.key)))
                            })
                            .map(|node| node.key.clone())
                            .collect();
                        this.children.insert(parent, nodes);
                        // The bound also prevents an extension that returns
                        // itself as a child from growing a tree forever.
                        if this.children.len() < 256 {
                            for key in expanded {
                                this.opened.insert(key.clone());
                                this.fetch(Some(key), cx);
                            }
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                this.rebuild();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        for (key, view) in &self.views {
            rows.push(Row::View(key.clone(), view.clone()));
            if Some(key) != self.active.as_ref() {
                continue;
            }
            if let Some(message) = &view.message {
                rows.push(Row::Note(message.clone()));
            }
            if let Some(nodes) = self.children.get(&None) {
                let mut subtree = Vec::new();
                self.append(nodes, 0, &mut subtree);
                rows.extend(subtree);
            } else if self.loading.contains(&None) {
                rows.push(Row::Note("Loading…".into()));
            }
            if view.kind == "webview" {
                rows.push(Row::Note("Open in a tab".into()));
            } else if self.children.get(&None).is_some_and(Vec::is_empty) {
                rows.push(Row::Note("No items".into()));
            }
            if let Some(error) = &self.error {
                rows.push(Row::Note(error.clone()));
            }
        }
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn append(&self, nodes: &[Node], depth: usize, out: &mut Vec<Row>) {
        if depth > 32 {
            return;
        }
        for node in nodes {
            if out.len() >= 10000 {
                return;
            }
            out.push(Row::Node(node.clone(), depth));
            if self.opened.contains(&node.key) {
                if let Some(children) = self.children.get(&Some(node.key.clone())) {
                    self.append(children, depth + 1, out);
                } else if self.loading.contains(&Some(node.key.clone())) {
                    out.push(Row::Note("Loading…".into()));
                }
            }
        }
    }

    fn toggle(&mut self, node: &Node, open: bool, cx: &mut Context<Self>) {
        if !node.opens {
            return;
        }
        if open {
            self.closed.remove(&node.key);
            self.opened.insert(node.key.clone());
            self.fetch(Some(node.key.clone()), cx);
        } else {
            self.opened.remove(&node.key);
            self.closed.insert(node.key.clone());
        }
        if let Some((_, id)) = &self.active {
            self.request(
                "view.expand",
                json!({"id": id, "node": node.key, "open": open}),
                cx,
            );
        }
        self.rebuild();
        cx.notify();
    }
    fn run(&mut self, run: String, cx: &mut Context<Self>) {
        if let Some((_, id)) = &self.active {
            self.request("view.run", json!({"id": id, "run": run}), cx);
        }
    }
    fn click(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index;
        match self.rows.get(index).cloned() {
            Some(Row::View(key, _)) => self.choose(key, cx),
            Some(Row::Node(node, _)) => {
                if node.opens {
                    self.toggle(&node, !self.opened.contains(&node.key), cx);
                } else if let Some(run) = node.run {
                    self.run(run, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }
    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Center);
        cx.notify();
    }
    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Center);
        cx.notify();
    }
    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(Row::Node(Node { run: Some(run), .. }, _)) = self.rows.get(self.selected) {
            self.run(run.clone(), cx);
        } else {
            self.click(self.selected, cx);
        }
    }
    fn run_selected(&mut self, _: &Run, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(Row::Node(node, _)) = self.rows.get(self.selected)
            && let Some(action) = node.actions.first()
        {
            self.run(action.run.clone(), cx);
        }
    }
    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(Row::Node(node, _)) = self.rows.get(self.selected).cloned() {
            self.toggle(&node, true, cx);
        } else {
            self.click(self.selected, cx);
        }
    }
    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(Row::Node(node, _)) = self.rows.get(self.selected).cloned() {
            self.toggle(&node, false, cx);
        }
    }
    fn refresh_provider(&mut self, cx: &mut Context<Self>) {
        let Some((owner, id)) = self.active.clone() else {
            return;
        };
        let active = self.active.clone();
        let task = self.store.update(cx, |store, cx| {
            store.ask_host(&owner, "view.refresh", json!({"id": id}), cx)
        });
        cx.spawn(async move |this, cx| {
            let answer = task.await;
            this.update(cx, |this, cx| {
                if this.active != active {
                    return;
                }
                match answer {
                    Ok(_) => this.refresh(cx),
                    Err(error) => {
                        this.error = Some(error);
                        this.rebuild();
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }
    fn refresh_action(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.refresh_provider(cx);
    }
}

impl Focusable for ExtensionViews {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for ExtensionViews {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        div()
            .size_full().flex().flex_col()
            .key_context("ExtensionViews").track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::run_selected))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::collapse))
            .on_action(cx.listener(Self::refresh_action))
            .child(
                div().px_2().py_1().flex().items_center().justify_between()
                    .text_size(UI_FONT_SIZE).text_color(theme.fg_muted)
                    .child("Extension views")
                    .when(self.active.is_some(), |d| d.child(ui::button(
                        "refresh-views", "Refresh", false, &theme,
                        cx.listener(|this, _, _, cx| this.refresh_provider(cx)),
                    ))),
            )
            .when(self.views.is_empty(), |d| d.child(
                div().p_3().text_size(UI_FONT_SIZE).text_color(theme.fg_muted)
                    .child("Views appear here when an enabled extension provides a tree, source control or tests."),
            ))
            .child(
                uniform_list("extension-views", self.rows.len(), cx.processor(
                    |this, range: std::ops::Range<usize>, window, cx| {
                        let theme = cx.theme().clone();
                        let focused = this.focus.contains_focused(window, cx);
                        range.map(|index| {
                            let row = this.rows[index].clone();
                            let header = matches!(row, Row::View(..));
                            let (label, detail, depth, icon, actions, tooltip, strike, mark) = match row {
                                Row::View(key, view) => (
                                    view.title, Some(key.0), 0,
                                    Some(match view.kind.as_str() { "scm" => "git-branch", "tests" => "check", _ => "folder" }),
                                    Vec::new(), None, false, None,
                                ),
                                Row::Node(node, depth) => {
                                    let detail = match (&node.mark, node.description) {
                                        (Some(mark), Some(description)) => Some(format!("{mark} · {description}")),
                                        (_, description) => node.mark.clone().or(description),
                                    };
                                    (node.label, detail, depth + 1,
                                        Some(if node.opens { if this.opened.contains(&node.key) { "arrow-down" } else { "arrow-right" } } else { "file" }),
                                        node.actions, node.tooltip, node.strike, node.mark)
                                }
                                Row::Note(text) => {
                                    let icon = if this.error.as_ref() == Some(&text) { Some("warning-circle") }
                                        else if text == "Loading…" { Some("spinner-gap") } else { None };
                                    (text, None, 1, icon, Vec::new(), None, false, None)
                                }
                            };
                            let color = match mark.as_deref() {
                                Some("Passed") => theme.git_added,
                                Some("Failed" | "Error") => theme.error,
                                Some("Running" | "Queued") => theme.accent,
                                _ => theme.fg_subtle,
                            };
                            div().id(("view-row", index)).w_full()
                                .debug_selector(move || format!("extension-view-{index}"))
                                .h(crate::theme::row(px(40.), cx))
                                .pl(px(8. + depth as f32 * 14.)).pr_2()
                                .flex().items_center().gap_1p5()
                                .text_size(UI_FONT_SIZE).text_color(theme.fg)
                                .when(this.selected == index, |d| d.bg(if focused { theme.accent_soft } else { theme.bg_elev }))
                                .hover(|d| d.bg(theme.bg_elev))
                                .children(icon.map(crate::icons::draw))
                                .child(
                                    div().flex_1().min_w_0().overflow_hidden()
                                        .child(div().truncate().when(header, |d| d.font_weight(gpui::FontWeight::SEMIBOLD))
                                            .when(strike, |d| d.line_through()).child(label))
                                        .children(detail.map(|text| div().truncate()
                                            .text_size(crate::theme::UI_FONT_SMALL).text_color(color).child(text))),
                                )
                                .when_some(tooltip, |d, tip| d.tooltip(move |_, cx| cx.new(|_| ui::Tooltip(tip.clone())).into()))
                                .children(actions.into_iter().enumerate().map(|(i, action)| {
                                    ui::button(("view-action", index * 100 + i), action.title, false, &theme,
                                        cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.run(action.run.clone(), cx); }))
                                        .debug_selector(move || format!("extension-view-action-{index}-{i}"))
                                }))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    window.focus(&this.focus); this.click(index, cx);
                                }))
                        }).collect()
                    },
                )).track_scroll(self.scroll.clone()).flex_1().min_h_0(),
            )
    }
}
