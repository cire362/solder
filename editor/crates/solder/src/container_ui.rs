//! Consent for an already parsed configuration. No Docker or filesystem work
//! runs here. The parent must review again if executable configuration changes,
//! including its referenced Dockerfile or Compose files, before using acceptance.

use gpui::{
    AnyElement, App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, KeyBinding,
    ListAlignment, ListState, Window, actions, div, list, prelude::*, px,
};

use crate::{
    devcontainer::{Config, Lifecycle, LifecycleCommand, ShutdownAction, Source},
    lang::{self, t, tf},
    theme::{self, ActiveTheme, UI_FONT_SIZE, UI_FONT_SMALL},
    ui,
};

actions!(container_ui, [OpenDevContainer, Approve, Cancel]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, Some("ContainerReview")),
        KeyBinding::new("ctrl-enter", Approve, Some("ContainerReview")),
        KeyBinding::new("cmd-enter", Approve, Some("ContainerReview")),
        KeyBinding::new("enter", Approve, Some("ContainerReviewApprove")),
        KeyBinding::new("enter", Cancel, Some("ContainerReviewCancel")),
    ]);
}

#[derive(Clone, Debug)]
pub enum ReviewEvent {
    /// The exact snapshot shown in this review, not a subsequently loaded file.
    Approved(Box<Config>),
    Cancelled,
}

enum Row {
    Field(&'static str, String),
    Masked(&'static str, String),
    Heading(Heading),
    Notice(Notice),
    Limitation(&'static str),
}

enum Heading {
    Environment,
    Lifecycle,
    Limitations,
}

enum Notice {
    ComposeMount,
    AllServices,
    NoEnvironment,
    HiddenValues,
    NoLifecycle,
    ReadOnly,
    ReadWrite,
    BindMount,
}

/// A native ManagedView: subscribe to ReviewEvent and DismissEvent. The parent
/// starts work only on Approved, using that Config, on a background thread.
pub struct Review {
    config: Option<Config>,
    rows: Vec<Row>,
    target: (&'static str, String),
    list: ListState,
    focus: FocusHandle,
    metrics: Option<(lang::Language, gpui::Pixels, gpui::Pixels, gpui::Pixels)>,
}

impl EventEmitter<ReviewEvent> for Review {}
impl EventEmitter<DismissEvent> for Review {}

impl Review {
    // Real reviews also include snapshots of the execution files.
    #[cfg(test)]
    pub fn new(config: Config, cx: &mut Context<Self>) -> Self {
        Self::with_details(config, serde_json::Value::Null, cx)
    }

    /// Details are read from the same file snapshot as Config, off the UI thread.
    /// Config's resolved paths and commands remain authoritative.
    pub fn with_details(
        config: Config,
        details: serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut rows = review_rows(&config);
        if let Some(environment) = details["remoteEnv"].as_object() {
            for key in environment.keys() {
                rows.push(Row::Masked("remoteEnv", key.clone()));
            }
        }
        for key in [
            "remoteUser",
            "containerUser",
            "runArgs",
            "overrideCommand",
            "init",
        ] {
            if let Some(value) = details.get(key) {
                rows.push(Row::Field(
                    key,
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned),
                ));
            }
        }
        let target = match config.source() {
            Source::Image(image) => ("image", image.clone()),
            Source::Dockerfile { dockerfile, .. } => {
                ("build.dockerfile", dockerfile.display().to_string())
            }
            Source::Compose { service, .. } => ("service", service.clone()),
        };
        Self {
            config: Some(config),
            list: ListState::new(rows.len(), ListAlignment::Top, px(0.)),
            rows,
            target,
            focus: cx.focus_handle(),
            metrics: None,
        }
    }

    fn approve(&mut self, _: &Approve, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(config) = self.config.take() {
            cx.emit(ReviewEvent::Approved(Box::new(config)));
            cx.emit(DismissEvent);
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self.config.take().is_some() {
            cx.emit(ReviewEvent::Cancelled);
            cx.emit(DismissEvent);
        }
    }

    fn render_row(&self, ix: usize, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let row = div()
            .w_full()
            .px_4()
            .py_2()
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .whitespace_normal();
        match &self.rows[ix] {
            Row::Field(label, value) | Row::Masked(label, value) => row
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(UI_FONT_SMALL)
                        .text_color(theme.fg_muted)
                        .child(field_label(label)),
                )
                .child(
                    div()
                        .id(("container-review-value", ix))
                        .w_full()
                        .overflow_x_scroll()
                        .child(if matches!(self.rows[ix], Row::Masked(_, _)) {
                            tf("{} = [hidden]", &[value])
                        } else {
                            value.clone()
                        }),
                )
                .into_any_element(),
            Row::Heading(heading) => row
                .pt_4()
                .border_t(theme.shape.border)
                .border_color(theme.line)
                .child(match heading {
                    Heading::Environment => t("Container environment"),
                    Heading::Lifecycle => t("Lifecycle commands"),
                    Heading::Limitations => t("Limitations"),
                })
                .into_any_element(),
            Row::Notice(notice) => row
                .text_color(theme.fg_muted)
                .child(match notice {
                    Notice::ComposeMount => {
                        t("Compose defines the workspace mount. It is resolved after approval.")
                    }
                    Notice::AllServices => t("All Compose services may be created and started."),
                    Notice::NoEnvironment => t("No container environment overrides"),
                    Notice::HiddenValues => t("Environment and build argument values are hidden."),
                    Notice::NoLifecycle => t("No lifecycle commands"),
                    Notice::ReadOnly => t("Workspace mount is read only"),
                    Notice::ReadWrite => t("Workspace mount allows writes"),
                    Notice::BindMount => t("Workspace access requires an inspected bind mount to an existing project directory. Named volumes are not supported."),
                })
                .into_any_element(),
            Row::Limitation(value) => row
                .text_color(theme.fg_muted)
                .child(limitation_text(value))
                .into_any_element(),
        }
    }
}

fn field_label(label: &'static str) -> &'static str {
    match label {
        "Workspace" => t("Workspace"),
        "Configuration file" => t("Configuration file"),
        "Mount source" => t("Mount source"),
        "Mount destination" => t("Mount destination"),
        other => other,
    }
}

fn lifecycle_name(stage: Lifecycle) -> &'static str {
    match stage {
        Lifecycle::OnCreate => "onCreateCommand",
        Lifecycle::UpdateContent => "updateContentCommand",
        Lifecycle::PostCreate => "postCreateCommand",
        Lifecycle::PostStart => "postStartCommand",
        Lifecycle::PostAttach => "postAttachCommand",
    }
}

fn review_rows(config: &Config) -> Vec<Row> {
    let mut rows = vec![
        Row::Field("Workspace", config.workspace().display().to_string()),
        Row::Field("Configuration file", config.file().display().to_string()),
        Row::Field("workspaceFolder", config.working_folder().to_owned()),
    ];
    if let Some(name) = config.name() {
        rows.push(Row::Field("name", name.to_owned()));
    }
    match config.source() {
        Source::Image(_) => {}
        Source::Dockerfile {
            context,
            args,
            target,
            ..
        } => {
            rows.push(Row::Field("build.context", context.display().to_string()));
            if let Some(target) = target {
                rows.push(Row::Field("build.target", target.clone()));
            }
            for key in args.keys() {
                rows.push(Row::Masked("build.args", key.clone()));
            }
        }
        Source::Compose {
            files,
            run_services,
            ..
        } => {
            for file in files {
                rows.push(Row::Field("dockerComposeFile", file.display().to_string()));
            }
            if run_services.is_empty() {
                rows.push(Row::Notice(Notice::AllServices));
            } else {
                for service in run_services {
                    rows.push(Row::Field("runServices", service.clone()));
                }
            }
        }
    }
    if let Some(mount) = config.workspace_mount() {
        rows.push(Row::Field(
            "Mount source",
            mount.source.display().to_string(),
        ));
        rows.push(Row::Field("Mount destination", mount.target.clone()));
        rows.push(Row::Notice(if mount.readonly {
            Notice::ReadOnly
        } else {
            Notice::ReadWrite
        }));
        if let Some(consistency) = &mount.consistency {
            rows.push(Row::Field("consistency", consistency.clone()));
        }
    } else {
        rows.push(Row::Notice(Notice::ComposeMount));
    }
    rows.push(Row::Heading(Heading::Environment));
    rows.push(Row::Notice(Notice::HiddenValues));
    if config.container_env().is_empty() {
        rows.push(Row::Notice(Notice::NoEnvironment));
    }
    for key in config.container_env().keys() {
        rows.push(Row::Masked("containerEnv", key.clone()));
    }
    rows.push(Row::Heading(Heading::Lifecycle));
    if config.lifecycle().is_empty() {
        rows.push(Row::Notice(Notice::NoLifecycle));
    }
    for (stage, command) in config.lifecycle() {
        let text = match command {
            LifecycleCommand::Shell(script) => script.clone(),
            // JSON keeps argv boundaries visible without suggesting a host shell.
            LifecycleCommand::Argv(argv) => {
                serde_json::to_string(argv).expect("string argv is JSON")
            }
        };
        rows.push(Row::Field(lifecycle_name(*stage), text));
    }
    rows.push(Row::Field(
        "shutdownAction",
        match config.shutdown_action() {
            ShutdownAction::None => "none",
            ShutdownAction::StopContainer => "stopContainer",
            ShutdownAction::StopCompose => "stopCompose",
        }
        .to_owned(),
    ));
    rows.push(Row::Heading(Heading::Limitations));
    rows.push(Row::Notice(Notice::BindMount));
    rows.extend(
        config
            .limitations()
            .iter()
            .map(|value| Row::Limitation(value)),
    );
    rows
}

fn limitation_text(text: &'static str) -> &'static str {
    match text {
        "No Features, extra mounts, port forwarding, customizations, image metadata or host lifecycle commands" => {
            t(
                "No Features, extra mounts, port forwarding, customizations, image metadata or host lifecycle commands",
            )
        }
        "No UID/GID rewriting or user shell environment probing, including their specification defaults" => {
            t(
                "No UID/GID rewriting or user shell environment probing, including their specification defaults",
            )
        }
        "Only project-local existing host paths; no Docker socket, external volumes or external networks" => {
            t(
                "Only project-local existing host paths; no Docker socket, external volumes or external networks",
            )
        }
        "Lifecycle stages are synchronous; parent schedules creation, content updates, starts and attachments" => {
            t(
                "Lifecycle commands run in order when the container is created, updated, started or attached.",
            )
        }
        "Compose requires config --format json; service scaling, profiles and runtime overrides are unsupported" => {
            t(
                "Compose requires config --format json; service scaling, profiles and runtime overrides are unsupported",
            )
        }
        other => other,
    }
}

impl Focusable for Review {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Review {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let row = theme::row(crate::picker::ROW_HEIGHT, cx);
        let metrics = (
            lang::current(),
            window.rem_size(),
            row,
            window.viewport_size().width,
        );
        // Wrapped rows have cached heights. Language, font or width changes
        // invalidate those heights before the list lays out visible rows.
        if self.metrics != Some(metrics) {
            self.list.reset(self.rows.len());
            self.metrics = Some(metrics);
        }
        div()
            .key_context("ContainerReview")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::approve))
            .on_action(cx.listener(Self::cancel))
            .w(theme::text(600.))
            .max_w_full()
            .max_h_full()
            .flex()
            .flex_col()
            .rounded(theme.shape.panel)
            .border(theme.shape.border)
            .border_color(theme.line)
            .bg(theme.bg_elev)
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .border_b(theme.shape.border)
                    .border_color(theme.line)
                    .child(t("Review dev container"))
                    .child(
                        div()
                            .text_size(UI_FONT_SMALL)
                            .text_color(theme.fg_muted)
                            .child(self.target.0),
                    )
                    .child(
                        div()
                            .id("container-review-target")
                            .w_full()
                            .overflow_x_scroll()
                            .child(self.target.1.clone()),
                    )
                    .child(
                        div().whitespace_normal().child(t("Approval may build an image, create and start containers, and run project commands.")),
                    ),
            )
            .child(
                list(self.list.clone(), cx.processor(|this, ix, _, cx| this.render_row(ix, cx)))
                    .w_full()
                    .h(row * 10.),
            )
            .child(
                div()
                    .flex_none()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .border_t(theme.shape.border)
                    .border_color(theme.line)
                    .child(div().whitespace_normal().text_size(UI_FONT_SMALL).text_color(theme.fg_muted).child(t("Approval applies to this configuration. Changes require another review.")))
                    .child(
                        div().flex().justify_end().gap_2()
                            .child(ui::button("container-review-cancel", t("Cancel"), false, &theme, cx.listener(|this, _, window, cx| this.cancel(&Cancel, window, cx)))
                                .focusable().key_context("ContainerReviewCancel"))
                            .child(ui::button("container-review-approve", t("Approve and continue"), true, &theme, cx.listener(|this, _, window, cx| this.approve(&Approve, window, cx)))
                                .focusable().key_context("ContainerReviewApprove")),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        collections::BTreeMap,
        path::PathBuf,
        rc::Rc,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "solder-container-ui-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("devcontainer.json"), "{}").unwrap();
            Self(dir.canonicalize().unwrap())
        }

        fn config(&self, source: &str) -> Config {
            Config::parse(
                source,
                &self.0.join("devcontainer.json"),
                &self.0,
                BTreeMap::new(),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn review_keeps_command_text_and_mounts_and_masks_environment_values() {
        let fixture = Fixture::new();
        let script = "printf '%s' 'raw & text'\nprintf done";
        let config = fixture.config(&serde_json::json!({"image":"example/image:tag", "containerEnv":{"TOKEN":"secret-value"}, "postCreateCommand":script, "postAttachCommand":["echo", "a b", "$literal"]}).to_string());
        let rows = review_rows(&config);
        let field = |key| {
            rows.iter()
                .find_map(|row| match row {
                    Row::Field(label, value) if *label == key => Some(value.as_str()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(field("postCreateCommand"), script);
        assert_eq!(
            field("postAttachCommand"),
            "[\"echo\",\"a b\",\"$literal\"]"
        );
        assert_eq!(field("Mount source"), fixture.0.to_str().unwrap());
        assert!(field("Mount destination").starts_with('/'));
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::Masked("containerEnv", key) if key == "TOKEN"))
        );
        assert!(
            rows.iter()
                .all(|row| !matches!(row, Row::Field(_, value) if value.contains("secret-value")))
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, Row::Limitation(_)))
                .count(),
            config.limitations().len()
        );
    }

    #[test]
    fn dockerfile_review_keeps_paths_and_target_and_hides_build_values() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("Dockerfile"), "FROM example/base").unwrap();
        let config = fixture.config(r#"{"build":{"dockerfile":"Dockerfile","context":".","target":"development","args":{"TOKEN":"build-secret"}}}"#);
        let rows = review_rows(&config);
        assert!(rows.iter().any(|row| matches!(row, Row::Field("build.context", value) if value == fixture.0.to_str().unwrap())));
        assert!(
            rows.iter().any(
                |row| matches!(row, Row::Field("build.target", value) if value == "development")
            )
        );
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::Masked("build.args", key) if key == "TOKEN"))
        );
        assert!(
            rows.iter()
                .all(|row| !matches!(row, Row::Field(_, value) if value.contains("build-secret")))
        );
    }

    #[test]
    fn compose_review_lists_files_and_services_without_resolving_them() {
        let fixture = Fixture::new();
        // Contents deliberately cannot be resolved as Compose. Review uses
        // only the validated paths; resolution remains behind acceptance.
        std::fs::write(fixture.0.join("compose.yaml"), "not a Compose model").unwrap();
        let config = fixture.config(r#"{"dockerComposeFile":"compose.yaml","service":"app","runServices":["app","db"],"workspaceFolder":"/work"}"#);
        let rows = review_rows(&config);
        assert!(rows.iter().any(|row| matches!(row, Row::Field("dockerComposeFile", value) if value == fixture.0.join("compose.yaml").to_str().unwrap())));
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::Field("runServices", value) if value == "db"))
        );
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::Notice(Notice::ComposeMount)))
        );
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::Notice(Notice::BindMount)))
        );
    }

    #[gpui::test]
    fn approval_emits_the_reviewed_snapshot_once(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture::new();
        let config = fixture.config("{\"image\":\"example/reviewed:tag\"}");
        let (review, cx) = cx.add_window_view(|window, cx| {
            theme::put(theme::Theme::dark(), window, cx);
            bind_keys(cx);
            let review = Review::new(config, cx);
            review.focus.focus(window);
            review
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let received = events.clone();
        let subscription = cx.update(|_, cx| {
            cx.subscribe(&review, move |_, event: &ReviewEvent, _| {
                received.borrow_mut().push(event.clone())
            })
        });
        cx.simulate_keystrokes("enter");
        assert!(events.borrow().is_empty());
        cx.simulate_keystrokes("ctrl-enter");
        review.update_in(cx, |review, window, cx| {
            review.approve(&Approve, window, cx);
            review.cancel(&Cancel, window, cx);
        });
        let events = events.borrow();
        assert_eq!(events.len(), 1);
        let ReviewEvent::Approved(config) = &events[0] else {
            panic!("expected approval")
        };
        assert_eq!(
            config.source(),
            &Source::Image("example/reviewed:tag".into())
        );
        drop(subscription);
    }

    #[gpui::test]
    fn cancellation_cannot_be_followed_by_approval(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture::new();
        let config = fixture.config("{\"image\":\"example/cancelled:tag\"}");
        let (review, cx) = cx.add_window_view(|window, cx| {
            theme::put(theme::Theme::dark(), window, cx);
            bind_keys(cx);
            let review = Review::new(config, cx);
            review.focus.focus(window);
            review
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let received = events.clone();
        let subscription = cx.update(|_, cx| {
            cx.subscribe(&review, move |_, event: &ReviewEvent, _| {
                received.borrow_mut().push(event.clone())
            })
        });
        cx.simulate_keystrokes("escape");
        review.update_in(cx, |review, window, cx| {
            review.approve(&Approve, window, cx);
        });
        assert!(matches!(
            events.borrow().as_slice(),
            [ReviewEvent::Cancelled]
        ));
        drop(subscription);
    }
}
