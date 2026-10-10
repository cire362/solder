//! Native browser children. GPUI lays out their rectangle; the extension
//! owns their content. No browser or polling task exists until a page opens.
use crate::{
    extension_store::ExtensionStore,
    settings::Settings,
    theme::{ActiveTheme, UI_FONT_SIZE},
};
use gpui::{
    App, Bounds, Context, Element, ElementId, Entity, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Style, Window, div, prelude::*, relative,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use wry::{
    WebView, WebViewBuilder,
    http::{Request, Response},
    raw_window_handle::HasWindowHandle,
};

pub type Key = (String, String);
const PAGE_LIMIT: usize = 8 * 1024 * 1024;
const RESOURCE_LIMIT: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Options {
    enable_scripts: bool,
    local_resource_roots: Vec<PathBuf>,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Model {
    pub view_type: String,
    pub document_uri: Option<String>,
    pub initial_state: Value,
    pub title: String,
    pub html: String,
    pub column: i32,
    options: Options,
}

impl Model {
    pub fn read(value: Value) -> Option<Self> {
        let model: Self = serde_json::from_value(value).ok()?;
        (model.html.len() <= PAGE_LIMIT && model.options.local_resource_roots.len() <= 64)
            .then_some(model)
    }
}

#[derive(Default)]
struct Source {
    model: Model,
    state: Value,
    theme: Value,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Input {
    Ready,
    State { value: Value },
    Message { value: Value },
    Key { key: String },
}

pub enum PageEvent {
    Key(String),
}
impl gpui::EventEmitter<PageEvent> for Page {}

pub struct Page {
    pub key: Key,
    pub model: Model,
    source: Arc<Mutex<Source>>,
    browser: Option<WebView>,
    inbox: mpsc::Receiver<(usize, Input)>,
    sender: mpsc::SyncSender<(usize, Input)>,
    generation: usize,
    pending: VecDeque<Value>,
    ready: bool,
    visible: bool,
    requested: bool,
    pub error: Option<String>,
}

impl Page {
    pub(crate) fn state(&self) -> Value {
        self.source.lock().unwrap().state.clone()
    }

    pub(crate) fn options(&self) -> Value {
        serde_json::to_value(&self.model.options).unwrap_or_default()
    }
    pub fn new(key: Key, model: Model, cx: &mut Context<Self>) -> Self {
        let (sender, inbox) = mpsc::sync_channel(128);
        cx.on_release(|page, cx| {
            let key = page.key.clone();
            cx.defer(move |cx| {
                if let Some(store) = ExtensionStore::try_global(cx)
                    && store.read(cx).api.webviews.contains_key(&key)
                {
                    store.update(cx, |store, cx| store.close_webview(&key, cx));
                }
            });
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(25))
                    .await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            key,
            model: model.clone(),
            source: Arc::new(Mutex::new(Source {
                state: model.initial_state.clone(),
                model,
                ..Default::default()
            })),
            browser: None,
            inbox,
            sender,
            generation: 0,
            pending: VecDeque::new(),
            ready: false,
            visible: false,
            requested: false,
            error: None,
        }
    }

    pub fn sync(&mut self, model: Model, cx: &mut Context<Self>) {
        if self.model == model {
            return;
        }
        let reload = self.model.html != model.html
            || self.model.options.enable_scripts != model.options.enable_scripts;
        self.model = model.clone();
        self.source.lock().unwrap().model = model;
        if reload {
            self.generation += 1;
            // Recreate for script-policy changes too: a page that enabled
            // scripts must not survive the extension disabling them.
            self.browser = None;
            self.requested = false;
            self.ready = false;
            self.error = None;
        }
        cx.notify();
    }

    pub fn show(&mut self, visible: bool) {
        if self.visible != visible {
            self.visible = visible;
            if let Some(browser) = &self.browser {
                let _ = browser.set_visible(visible);
            }
        }
    }

    pub fn focus(&self) {
        if let Some(browser) = &self.browser {
            let _ = browser.focus();
        }
    }

    pub fn post(&mut self, message: Value) -> bool {
        if self.pending.len() >= 256
            || message.to_string().len() > PAGE_LIMIT
            || self.error.is_some()
        {
            return false;
        }
        self.pending.push_back(message);
        self.flush();
        true
    }

    fn flush(&mut self) {
        if !self.ready {
            return;
        }
        let Some(browser) = &self.browser else {
            return;
        };
        while let Some(message) = self.pending.pop_front() {
            let script =
                format!("window.dispatchEvent(new MessageEvent('message',{{data:{message}}}));");
            if browser.evaluate_script(&script).is_err() {
                self.pending.clear();
                break;
            }
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        while let Ok((generation, input)) = self.inbox.try_recv() {
            if generation != self.generation {
                continue;
            }
            match input {
                Input::Ready => {
                    self.ready = true;
                    self.flush();
                }
                Input::State { value } => self.source.lock().unwrap().state = value,
                Input::Message { value } => self.tell(
                    "webview.message",
                    json!({"id":self.key.1,"message":value}),
                    cx,
                ),
                Input::Key { key } => cx.emit(PageEvent::Key(key)),
            }
        }
        // GPUI owns the Linux loop. Pump GTK only while a browser exists,
        // so no GTK display is opened at startup or by headless tests.
        #[cfg(target_os = "linux")]
        if self.browser.is_some() {
            for _ in 0..16 {
                if !gtk::events_pending() {
                    break;
                }
                gtk::main_iteration_do(false);
            }
        }
    }

    fn tell(&self, method: &str, params: Value, cx: &App) {
        if let Some(store) = ExtensionStore::try_global(cx) {
            store.read(cx).webview_tell(&self.key.0, method, params);
        }
    }

    fn place(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        let theme = theme_values(cx);
        let changed = {
            let mut source = self.source.lock().unwrap();
            if source.theme == theme {
                false
            } else {
                source.theme = theme.clone();
                true
            }
        };
        let rectangle = wry::Rect {
            position: wry::dpi::LogicalPosition::new(
                f64::from(bounds.origin.x),
                f64::from(bounds.origin.y),
            )
            .into(),
            size: wry::dpi::LogicalSize::new(
                f64::from(bounds.size.width),
                f64::from(bounds.size.height),
            )
            .into(),
        };
        if let Some(browser) = &self.browser {
            let _ = browser.set_bounds(rectangle);
            if changed {
                let _ = browser.evaluate_script(&format!("window.__solderTheme({theme});"));
            }
            return;
        }
        if self.requested {
            return;
        }
        self.requested = true;
        // The windows of tests are backed by no window of the system, and
        // asking one for its handle panics: no browser is made in tests,
        // where a page is its model and what is sent to it.
        if cfg!(test) || HasWindowHandle::window_handle(window).is_err() {
            return;
        }
        match self.build(rectangle, window) {
            Ok(browser) => self.browser = Some(browser),
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn build(&self, bounds: wry::Rect, window: &Window) -> Result<WebView, String> {
        #[cfg(target_os = "linux")]
        {
            if !matches!(
                // The system's handle, by the trait's name: called as a
                // method, a window gives GPUI's own handle of the same name.
                HasWindowHandle::window_handle(window).map(|h| h.as_raw()),
                Ok(wry::raw_window_handle::RawWindowHandle::Xlib(_)
                    | wry::raw_window_handle::RawWindowHandle::Xcb(_))
            ) {
                return Err("Webviews need X11 on Linux. Start Solder with an X11 session.".into());
            }
            gtk::init().map_err(|e| e.to_string())?;
        }
        let source = self.source.clone();
        let sender = self.sender.clone();
        let id = self.key.1.clone();
        let url = page_url(&id);
        let expected = url.clone();
        let ipc_origin = url.clone();
        let generation = self.generation;
        let script = include_str!("webview.js");
        let mut builder = WebViewBuilder::new()
            .with_bounds(bounds)
            .with_visible(true)
            .with_focused(false)
            .with_incognito(true)
            .with_initialization_script_for_main_only(script, true)
            .with_ipc_handler(move |request| {
                if request.body().len() <= PAGE_LIMIT
                    && request.uri().to_string().split('#').next() == Some(ipc_origin.as_str())
                    && let Ok(input) = serde_json::from_str(request.body())
                {
                    let _ = sender.try_send((generation, input));
                }
            })
            .with_navigation_handler(move |destination| {
                destination == expected || destination.starts_with(&format!("{expected}#"))
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_permission_handler(|_| wry::PermissionResponse::Deny)
            .with_download_started_handler(|_, _| false);
        let html_source = source.clone();
        builder = builder.with_custom_protocol("solder-webview".into(), move |_, _| {
            let source = html_source.lock().unwrap();
            let config = json!({"state":source.state,"theme":source.theme});
            // Config is supplied in the initialization script's closure,
            // rather than an inline script that would weaken the page CSP.
            response(
                200,
                "text/html; charset=utf-8",
                format!(
                    "<!--solder-config:{}-->{}",
                    escape_config(&config),
                    source.model.html
                )
                .into_bytes(),
            )
        });
        let resource_id = id;
        builder = builder.with_asynchronous_custom_protocol(
            "solder-resource".into(),
            move |_, request, responder| {
                let source = source.clone();
                let id = resource_id.clone();
                std::thread::spawn(move || {
                    let roots = source
                        .lock()
                        .unwrap()
                        .model
                        .options
                        .local_resource_roots
                        .clone();
                    responder.respond(resource(&id, &roots, &request));
                });
            },
        );
        if !self.model.options.enable_scripts {
            builder = builder.with_javascript_disabled();
        }
        builder
            .with_url(url)
            .build_as_child(window)
            .map_err(|e| e.to_string())
    }
}

impl Render for Page {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.fg_muted)
            .text_size(UI_FONT_SIZE)
            .child(if let Some(error) = &self.error {
                div()
                    .p_4()
                    .child(format!("Could not open this page. {error}"))
                    .into_any_element()
            } else {
                BrowserElement(cx.entity()).into_any_element()
            })
    }
}

struct BrowserElement(Entity<Page>);
impl IntoElement for BrowserElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for BrowserElement {
    type RequestLayoutState = ();
    type PrepaintState = ();
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
    ) {
        self.0.update(cx, |page, cx| page.place(bounds, window, cx));
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }
}

fn page_url(id: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("http://solder-webview.{id}/index.html")
    } else {
        format!("solder-webview://{id}/index.html")
    }
}
fn escape_config(value: &Value) -> String {
    // A comment is data, not executable HTML, and it ends only at a `>`.
    // In JSON a `<` or `>` can stand in a string alone, where it may be
    // written by its number; a minus may also begin a number, where it
    // may not, so it is left as it is.
    value
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}
fn response(
    code: u16,
    content_type: &str,
    bytes: Vec<u8>,
) -> Response<std::borrow::Cow<'static, [u8]>> {
    Response::builder()
        .status(code)
        .header("Content-Type", content_type)
        .header("Access-Control-Allow-Origin", "*")
        .header("X-Content-Type-Options", "nosniff")
        .body(bytes.into())
        .unwrap()
}

fn resource(
    id: &str,
    roots: &[PathBuf],
    request: &Request<Vec<u8>>,
) -> Response<std::borrow::Cow<'static, [u8]>> {
    let host = request.uri().host().unwrap_or_default();
    if host != id && host != format!("solder-resource.{id}") || request.method() != "GET" {
        return response(403, "text/plain", Vec::new());
    }
    let Some(path) = decode_path(request.uri().path()) else {
        return response(400, "text/plain", Vec::new());
    };
    let Ok(path) = PathBuf::from(path).canonicalize() else {
        return response(404, "text/plain", Vec::new());
    };
    if !roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| path.starts_with(root))
    {
        return response(403, "text/plain", Vec::new());
    }
    let Ok(metadata) = path.metadata() else {
        return response(404, "text/plain", Vec::new());
    };
    if !metadata.is_file() || metadata.len() > RESOURCE_LIMIT {
        return response(413, "text/plain", Vec::new());
    }
    let content_type = match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
    {
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "html" => "text/html",
        _ => "application/octet-stream",
    };
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() as u64 <= RESOURCE_LIMIT => response(200, content_type, bytes),
        _ => response(404, "text/plain", Vec::new()),
    }
}
fn decode_path(path: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hi = char::from(bytes.next()?).to_digit(16)?;
            let lo = char::from(bytes.next()?).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
        } else {
            out.push(byte);
        }
    }
    String::from_utf8(out)
        .ok()
        .filter(|path| !path.contains('\0'))
}

fn theme_values(cx: &App) -> Value {
    let theme = cx.theme();
    let css = |color: gpui::Hsla| {
        let rgba: gpui::Rgba = color.into();
        format!(
            "rgba({},{},{},{})",
            (rgba.r * 255.).round(),
            (rgba.g * 255.).round(),
            (rgba.b * 255.).round(),
            rgba.a
        )
    };
    // The interface's font may be one only Solder has: the page falls back
    // to the system's, as a page in VS Code would, not to the browser's.
    let settings = Settings::get(cx);
    let named = |font: &str, rest: &str| format!("\"{}\", {rest}", font.replace(['"', '\\'], ""));
    let font = named(
        &settings.ui_font(),
        "-apple-system, BlinkMacSystemFont, \"Segoe UI\", sans-serif",
    );
    let code = named(
        &settings.buffer_font_family,
        "ui-monospace, Menlo, Consolas, monospace",
    );
    json!({"dark": theme.bg.l < 0.5, "font": font, "code": code, "size": settings.ui_font_size,
        "codeSize": settings.buffer_font_size,
        "colors": {"editor-background":css(theme.bg),"editor-foreground":css(theme.fg),
        "sideBar-background":css(theme.bg_sunken),"panel-border":css(theme.line),"focusBorder":css(theme.accent),
        "button-background":css(theme.accent),"button-foreground":css(theme.accent_fg),
        "input-background":css(theme.bg_elev),"input-foreground":css(theme.fg),"input-border":css(theme.line),
        "descriptionForeground":css(theme.fg_muted),"errorForeground":css(theme.error),
        "textLink-foreground":css(theme.accent),"editor-selectionBackground":css(theme.selection)}})
}

impl ExtensionStore {
    pub(crate) fn webview_said(
        &mut self,
        owner: &str,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = params["id"].as_str().filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        }) else {
            return;
        };
        let key = (owner.to_string(), id.to_string());
        if params["gone"] == true {
            self.api.webviews.remove(&key);
            self.api.web_posts.remove(&key);
        } else if method == "webview.reveal" {
            if self.api.webviews.contains_key(&key) {
                self.ask(crate::extension_api::Ask::Webview { key }, cx);
            }
        } else if let Some(model) = Model::read(params) {
            let new = self.api.webviews.insert(key.clone(), model).is_none();
            if new {
                self.ask(crate::extension_api::Ask::Webview { key }, cx);
            }
        }
        cx.emit(crate::extension_api::ExtensionEvent::Webviews);
    }

    pub(crate) fn webview_tell(&self, owner: &str, method: &str, params: Value) {
        if let Some(host) = self.code.get(owner).and_then(|code| code.host.as_ref()) {
            host.notify(method, params);
        }
    }

    pub(crate) fn close_webview(&mut self, key: &Key, cx: &mut Context<Self>) {
        self.api.webviews.remove(key);
        self.api.web_posts.remove(key);
        self.webview_tell(&key.0, "webview.closed", json!({"id":key.1}));
        cx.emit(crate::extension_api::ExtensionEvent::Webviews);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(uri: &str) -> Request<Vec<u8>> {
        Request::builder().uri(uri).body(Vec::new()).unwrap()
    }

    #[test]
    fn a_page_reads_only_files_under_the_folders_it_was_given() {
        let dir = db::testing::dir("webview-resources")
            .canonicalize()
            .unwrap();
        std::fs::create_dir_all(dir.join("media")).unwrap();
        std::fs::create_dir_all(dir.join("secret")).unwrap();
        std::fs::write(dir.join("media/a b.css"), "p{}").unwrap();
        std::fs::write(dir.join("secret/key.txt"), "key").unwrap();
        let roots = [dir.join("media")];
        let at = |path: &str| format!("solder-resource://page1{}", path.replace(' ', "%20"));
        let ask = |uri: &str| resource("page1", &roots, &get(uri));

        // A file under a folder it was given, by a name with a space in it.
        let found = ask(&at(&format!("{}/media/a b.css", dir.display())));
        assert_eq!(found.status(), 200);
        assert_eq!(found.headers()["Content-Type"], "text/css");
        assert_eq!(&**found.body(), b"p{}");
        // One outside them, and one reached by going up from inside.
        assert_eq!(
            ask(&at(&format!("{}/secret/key.txt", dir.display()))).status(),
            403
        );
        let up = at(&format!("{}/media/../secret/key.txt", dir.display()));
        assert_eq!(ask(&up).status(), 403);
        // One that is not there, a folder, and a name that is no text.
        assert_eq!(
            ask(&at(&format!("{}/media/none.css", dir.display()))).status(),
            404
        );
        assert_eq!(ask(&at(&format!("{}/media", dir.display()))).status(), 413);
        assert_eq!(ask("solder-resource://page1/%ff%fe").status(), 400);
        // Asked under the name of another page, nothing is given.
        let other = format!("solder-resource://page2{}/media/a%20b.css", dir.display());
        assert_eq!(ask(&other).status(), 403);
        let post = Request::builder()
            .method("POST")
            .uri(at(&format!("{}/media/a b.css", dir.display())))
            .body(Vec::new())
            .unwrap();
        assert_eq!(resource("page1", &roots, &post).status(), 403);
    }

    #[test]
    fn what_a_page_is_given_cannot_end_the_comment_it_comes_in() {
        let config = json!({ "state": "--><script>alert(1)</script>", "n": -1 });
        let escaped = escape_config(&config);
        assert!(!escaped.contains('<') && !escaped.contains('>'));
        // It is still the same value once read.
        assert_eq!(serde_json::from_str::<Value>(&escaped).unwrap(), config);
        assert_eq!(decode_path("/a%20b/%D0%B4"), Some("/a b/\u{434}".into()));
        assert_eq!(decode_path("/a%2"), None);
        assert_eq!(decode_path("/a%00b"), None);
    }

    #[test]
    fn a_page_too_large_or_with_too_many_folders_is_not_taken() {
        let page = |html: String, roots: usize| {
            let roots: Vec<String> = (0..roots).map(|i| format!("/r{i}")).collect();
            Model::read(json!({
                "title": "T", "html": html, "column": 2,
                "options": { "enableScripts": true, "localResourceRoots": roots },
            }))
        };
        let read = page("<p>hi</p>".into(), 2).unwrap();
        assert_eq!(
            (read.title.as_str(), read.column, read.html.as_str()),
            ("T", 2, "<p>hi</p>")
        );
        assert!(read.options.enable_scripts);
        assert!(page("x".repeat(PAGE_LIMIT + 1), 0).is_none());
        assert!(page(String::new(), 65).is_none());
        // What is left out is off.
        assert_eq!(Model::read(json!({})), Some(Model::default()));
    }
}
