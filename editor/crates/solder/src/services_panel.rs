//! The Services tab: the project's dev servers and its Docker containers.
//!
//! Each running service is a terminal in the bottom dock, so its logs are a
//! real TTY (colors, prompts, Ctrl+C). The panel keeps weak handles: closing
//! a service's terminal tab stops the service.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use gpui::{
    App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Task,
    WeakEntity, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::{
    services::{self, Container, ServiceSpec},
    terminal::Terminal,
    theme::{ActiveTheme, Theme, UI_FONT_SIZE},
    ui,
};

actions!(services, [RunStack, StopAll, RefreshServices]);

pub enum ServicesEvent {
    /// Start this service in a terminal; the workspace calls `attach` back.
    Start(ServiceSpec),
    /// Show a service's terminal in the dock.
    Reveal(Entity<Terminal>),
    /// A service ignored Ctrl+C; close its terminal, which hangs it up.
    Kill(Entity<Terminal>),
    /// Follow a container's logs in a terminal.
    ContainerLogs(Container),
}

impl EventEmitter<ServicesEvent> for ServicesPanel {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Running,
    Stopping,
    Exited(Option<i32>),
}

struct Run {
    terminal: WeakEntity<Terminal>,
    /// Ports seen in the output so far, plus the configured ones.
    ports: Vec<u16>,
    stopping: Option<Task<()>>,
    restart: bool,
}

#[derive(Clone)]
enum Row {
    Header(&'static str),
    Service(usize),
    Container(usize),
    Note(SharedString),
}

/// How long a service gets to exit after Ctrl+C before its terminal closes.
const STOP_GRACE: Duration = Duration::from_secs(3);
const ROW_HEIGHT: gpui::Pixels = px(28.);

pub struct ServicesPanel {
    root: PathBuf,
    specs: Vec<ServiceSpec>,
    detected: bool,
    error: Option<SharedString>,
    runs: HashMap<String, Run>,
    containers: Vec<Container>,
    containers_error: Option<SharedString>,
    containers_loaded: bool,
    visible: bool,
    focus_handle: FocusHandle,
    detect_task: Option<Task<()>>,
    containers_task: Option<Task<()>>,
    _poll: Task<()>,
}

impl ServicesPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        // Once a second: pick up announced ports and finished restarts. Every
        // third tick, refresh containers while the tab is on screen.
        let poll = cx.spawn(async move |this, cx| {
            let mut tick = 0u64;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                tick += 1;
                let alive = this
                    .update(cx, |this, cx| {
                        this.poll_runs(cx);
                        if this.visible && tick.is_multiple_of(3) {
                            this.refresh_containers(cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        let mut panel = Self {
            root,
            specs: Vec::new(),
            detected: false,
            error: None,
            runs: HashMap::new(),
            containers: Vec::new(),
            containers_error: None,
            containers_loaded: false,
            visible: false,
            focus_handle: cx.focus_handle(),
            detect_task: None,
            containers_task: None,
            _poll: poll,
        };
        panel.redetect(cx);
        panel
    }

    #[cfg(test)]
    pub fn specs(&self) -> &[ServiceSpec] {
        &self.specs
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        let appeared = visible && !self.visible;
        self.visible = visible;
        if appeared {
            self.refresh_containers(cx);
        }
    }

    /// Re-reads manifests. Called when one of them changes on disk.
    pub fn redetect(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.detect_task = Some(cx.spawn(async move |this, cx| {
            let (specs, error) = cx
                .background_executor()
                .spawn(async move { services::detect(&root) })
                .await;
            this.update(cx, |this, cx| {
                this.specs = specs;
                this.error = error.map(Into::into);
                this.detected = true;
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn refresh_containers(&mut self, cx: &mut Context<Self>) {
        self.containers_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { services::list_containers() })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(list) => {
                        this.containers = list;
                        this.containers_error = None;
                    }
                    Err(e) => {
                        this.containers.clear();
                        this.containers_error = Some(e.into());
                    }
                }
                this.containers_loaded = true;
                cx.notify();
            })
            .ok();
        }));
    }

    /// The workspace started `name` in `terminal`.
    pub fn attach(&mut self, name: &str, terminal: &Entity<Terminal>, cx: &mut Context<Self>) {
        let ports = self
            .specs
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.ports.clone())
            .unwrap_or_default();
        self.runs.insert(
            name.to_string(),
            Run {
                terminal: terminal.downgrade(),
                ports,
                stopping: None,
                restart: false,
            },
        );
        cx.notify();
    }

    pub fn status(&self, name: &str, cx: &App) -> Status {
        let Some(run) = self.runs.get(name) else {
            return Status::Stopped;
        };
        let Some(terminal) = run.terminal.upgrade() else {
            return Status::Stopped;
        };
        let t = terminal.read(cx);
        if t.exited {
            Status::Exited(t.exit_code)
        } else if run.stopping.is_some() {
            Status::Stopping
        } else {
            Status::Running
        }
    }

    /// Ports of services that are running now, first started first.
    pub fn running_ports(&self, cx: &App) -> Vec<u16> {
        let mut names: Vec<&String> = self.runs.keys().collect();
        names.sort();
        names
            .into_iter()
            .filter(|n| self.status(n, cx) == Status::Running)
            .flat_map(|n| self.ports(n).iter().copied())
            .collect()
    }

    pub fn ports(&self, name: &str) -> &[u16] {
        self.runs.get(name).map_or(&[], |r| &r.ports)
    }

    fn poll_runs(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut restart = Vec::new();
        self.runs.retain(|name, run| {
            let Some(terminal) = run.terminal.upgrade() else {
                changed = true;
                return false;
            };
            let t = terminal.read(cx);
            if t.exited {
                if run.restart {
                    restart.push(name.clone());
                }
                return true;
            }
            for port in services::ports_in(&t.visible_text().join("\n")) {
                if !run.ports.contains(&port) {
                    run.ports.push(port);
                    run.ports.sort_unstable();
                    changed = true;
                }
            }
            true
        });
        for name in restart {
            if let Some(run) = self.runs.get_mut(&name) {
                run.restart = false;
            }
            self.start_named(&name, cx);
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn start_named(&mut self, name: &str, cx: &mut Context<Self>) {
        if matches!(self.status(name, cx), Status::Running | Status::Stopping) {
            return;
        }
        if let Some(spec) = self.specs.iter().find(|s| s.name == name).cloned() {
            cx.emit(ServicesEvent::Start(spec));
        }
    }

    pub fn stop_named(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(run) = self.runs.get_mut(name) else {
            return;
        };
        let Some(terminal) = run.terminal.upgrade() else {
            return;
        };
        if terminal.read(cx).exited || run.stopping.is_some() {
            return;
        }
        terminal.read(cx).interrupt();
        let name = name.to_string();
        run.stopping = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STOP_GRACE).await;
            this.update(cx, |this, cx| {
                let Some(run) = this.runs.get_mut(&name) else {
                    return;
                };
                run.stopping = None;
                if let Some(terminal) = run.terminal.upgrade()
                    && !terminal.read(cx).exited
                {
                    cx.emit(ServicesEvent::Kill(terminal));
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn restart_named(&mut self, name: &str, cx: &mut Context<Self>) {
        match self.status(name, cx) {
            Status::Running | Status::Stopping => {
                if let Some(run) = self.runs.get_mut(name) {
                    run.restart = true;
                }
                self.stop_named(name, cx);
            }
            _ => self.start_named(name, cx),
        }
    }

    /// Starts every service that is not running.
    pub fn start_all(&mut self, cx: &mut Context<Self>) {
        let names: Vec<String> = self.specs.iter().map(|s| s.name.clone()).collect();
        for name in names {
            self.start_named(&name, cx);
        }
    }

    pub fn stop_all_services(&mut self, cx: &mut Context<Self>) {
        let names: Vec<String> = self.runs.keys().cloned().collect();
        for name in names {
            self.stop_named(&name, cx);
        }
    }

    fn run_stack(&mut self, _: &RunStack, _: &mut Window, cx: &mut Context<Self>) {
        self.start_all(cx);
    }

    fn stop_all(&mut self, _: &StopAll, _: &mut Window, cx: &mut Context<Self>) {
        self.stop_all_services(cx);
    }

    fn refresh(&mut self, _: &RefreshServices, _: &mut Window, cx: &mut Context<Self>) {
        self.redetect(cx);
        self.refresh_containers(cx);
    }

    fn container_action(&mut self, id: String, action: &'static str, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { services::container_action(&id, action) })
                .await;
            this.update(cx, |this, cx| {
                this.containers_error = result.err().map(Into::into);
                this.refresh_containers(cx);
            })
            .ok();
        })
        .detach();
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Header("Services")];
        if !self.detected {
            rows.push(Row::Note("Looking for services...".into()));
        } else if self.specs.is_empty() {
            rows.push(Row::Note(
                format!("No services found. Add them in {}.", services::CUSTOM_FILE).into(),
            ));
        }
        rows.extend((0..self.specs.len()).map(Row::Service));
        rows.push(Row::Header("Containers"));
        if let Some(e) = &self.containers_error {
            rows.push(Row::Note(e.clone()));
        } else if !self.containers_loaded {
            rows.push(Row::Note("Checking Docker...".into()));
        } else if self.containers.is_empty() {
            rows.push(Row::Note("No containers".into()));
        }
        rows.extend((0..self.containers.len()).map(Row::Container));
        rows
    }

    fn icon_button(
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        theme: &Theme,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex_none()
            .h(px(20.))
            .px_1p5()
            .flex()
            .items_center()
            .rounded(theme.shape.token)
            .text_size(crate::theme::text(11.))
            .text_color(theme.fg_subtle)
            .hover(|d| d.bg(theme.line).text_color(theme.fg))
            .child(label)
            .on_click(on_click)
    }

    fn port_chips(ports: &[u16], theme: &Theme) -> Vec<gpui::AnyElement> {
        ports
            .iter()
            .map(|port| {
                let url = format!("http://localhost:{port}");
                div()
                    .id(("port", *port as usize))
                    .flex_none()
                    .px_1p5()
                    .rounded(theme.shape.token)
                    .bg(theme.bg_elev)
                    .text_size(crate::theme::text(11.))
                    .text_color(theme.accent)
                    .hover(|d| d.bg(theme.accent_soft))
                    .child(format!(":{port}"))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            })
            .collect()
    }

    fn render_row(
        &self,
        ix: usize,
        row: &Row,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let base = div()
            .id(ix)
            .h(crate::theme::row(ROW_HEIGHT, cx))
            .mx_1p5()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(theme.shape.control)
            .text_size(UI_FONT_SIZE);
        match row {
            Row::Header(title) => base
                .justify_between()
                .text_size(crate::theme::text(11.5))
                .text_color(theme.fg_subtle)
                .child(*title)
                .into_any_element(),
            Row::Note(text) => base
                .text_color(theme.fg_subtle)
                .child(div().truncate().child(text.clone()))
                .into_any_element(),
            Row::Service(i) => {
                let spec = &self.specs[*i];
                let name = spec.name.clone();
                let status = self.status(&name, cx);
                let (dot, label) = match status {
                    Status::Running => (theme.git_added, "running".to_string()),
                    Status::Stopping => (theme.warning, "stopping".to_string()),
                    Status::Exited(Some(0)) => (theme.fg_subtle, "exited".to_string()),
                    Status::Exited(Some(code)) => (theme.error, format!("exited {code}")),
                    Status::Exited(None) => (theme.error, "killed".to_string()),
                    Status::Stopped => (theme.fg_subtle, spec.kind.label().to_string()),
                };
                let entity = cx.entity();
                let act = move |f: fn(&mut Self, &str, &mut Context<Self>)| {
                    let entity = entity.clone();
                    let name = name.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        entity.update(cx, |this, cx| f(this, &name, cx))
                    }
                };
                let running = matches!(status, Status::Running | Status::Stopping);
                let terminal = self.runs.get(&spec.name).and_then(|r| r.terminal.upgrade());
                let mut row = base
                    .group("service-row")
                    .hover(|d| d.bg(theme.accent_soft))
                    // A status dot is real state here: running, exited, failed.
                    .child(div().size(px(7.)).flex_none().rounded(px(4.)).bg(dot))
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.fg)
                            .child(spec.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::theme::text(11.))
                            .text_color(theme.fg_subtle)
                            .child(label),
                    )
                    .children(Self::port_chips(self.ports(&spec.name), theme))
                    .child(div().flex_1());
                if let Some(terminal) = terminal {
                    row = row.child(Self::icon_button(("logs", *i), "Logs", theme, {
                        let entity = cx.entity();
                        move |_, _, cx| {
                            let terminal = terminal.clone();
                            entity.update(cx, |_, cx| cx.emit(ServicesEvent::Reveal(terminal)))
                        }
                    }));
                }
                if running {
                    row = row
                        .child(Self::icon_button(
                            ("restart", *i),
                            "Restart",
                            theme,
                            act(Self::restart_named),
                        ))
                        .child(Self::icon_button(
                            ("stop", *i),
                            "Stop",
                            theme,
                            act(Self::stop_named),
                        ));
                } else {
                    row = row.child(Self::icon_button(
                        ("start", *i),
                        "Start",
                        theme,
                        act(Self::start_named),
                    ));
                }
                row.into_any_element()
            }
            Row::Container(i) => {
                let c = self.containers[*i].clone();
                let running = c.running();
                let entity = cx.entity();
                let action = move |action: &'static str| {
                    let entity = entity.clone();
                    let id = c.id.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        entity.update(cx, |this, cx| this.container_action(id.clone(), action, cx))
                    }
                };
                let logs = {
                    let entity = cx.entity();
                    let c = self.containers[*i].clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        entity.update(cx, |_, cx| cx.emit(ServicesEvent::ContainerLogs(c.clone())))
                    }
                };
                let c = &self.containers[*i];
                base.hover(|d| d.bg(theme.accent_soft))
                    .child(
                        div()
                            .size(px(7.))
                            .flex_none()
                            .rounded(px(4.))
                            .bg(if running {
                                theme.git_added
                            } else {
                                theme.fg_subtle
                            }),
                    )
                    .child(div().flex_none().text_color(theme.fg).child(c.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::theme::text(11.))
                            .text_color(theme.fg_subtle)
                            .child(format!("{}  {}", c.image, c.status)),
                    )
                    .child(Self::icon_button(("clogs", *i), "Logs", theme, logs))
                    .when(running, |d| {
                        d.child(Self::icon_button(
                            ("crestart", *i),
                            "Restart",
                            theme,
                            action("restart"),
                        ))
                        .child(Self::icon_button(
                            ("cstop", *i),
                            "Stop",
                            theme,
                            action("stop"),
                        ))
                    })
                    .when(!running, |d| {
                        d.child(Self::icon_button(
                            ("cstart", *i),
                            "Start",
                            theme,
                            action("start"),
                        ))
                    })
                    .into_any_element()
            }
        }
    }
}

impl Focusable for ServicesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ServicesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.rows().len();
        let any_running = self
            .runs
            .keys()
            .any(|name| matches!(self.status(name, cx), Status::Running));
        div()
            .key_context("ServicesPanel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::run_stack))
            .on_action(cx.listener(Self::stop_all))
            .on_action(cx.listener(Self::refresh))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .pb_2()
                    .flex()
                    .gap_1p5()
                    .child(
                        ui::button("run-stack", "Run stack", true, &theme, |_, window, cx| {
                            window.dispatch_action(Box::new(RunStack), cx)
                        })
                        .flex_1(),
                    )
                    .when(any_running, |d| {
                        d.child(ui::button(
                            "stop-all",
                            "Stop all",
                            false,
                            &theme,
                            |_, window, cx| window.dispatch_action(Box::new(StopAll), cx),
                        ))
                    })
                    .child(ui::button(
                        "services-refresh",
                        "Refresh",
                        false,
                        &theme,
                        |_, window, cx| window.dispatch_action(Box::new(RefreshServices), cx),
                    )),
            )
            .children(self.error.clone().map(|e| {
                div()
                    .mx_2()
                    .mb_2()
                    .px_2()
                    .py_1()
                    .rounded(theme.shape.control)
                    .border(theme.shape.border)
                    .border_color(theme.error)
                    .text_size(crate::theme::text(11.5))
                    .text_color(theme.error)
                    .child(e)
            }))
            .child(
                uniform_list(
                    "services",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let theme = cx.theme().clone();
                        let rows = this.rows();
                        range
                            .filter_map(|ix| {
                                rows.get(ix).map(|r| this.render_row(ix, r, &theme, cx))
                            })
                            .collect()
                    }),
                )
                .flex_1(),
            )
    }
}
