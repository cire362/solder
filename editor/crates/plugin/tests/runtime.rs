//! The host against a real plugin: `fixtures/probe`, built for wasm32.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use plugin::{
    Activity, Budget, EditorText, Host, HttpResponse, Manifest, Plugin, Reply, Request, Stats,
    testing,
};

/// An editor that records what plugins ask of it.
#[derive(Default)]
struct Editor {
    statuses: Mutex<Vec<String>>,
    requests: Mutex<Vec<Request>>,
}

impl Host for Editor {
    fn request(&self, _: &str, request: Request) -> Reply {
        self.requests.lock().unwrap().push(request.clone());
        let json = |value| Ok(serde_json::to_value(value).unwrap());
        match request {
            Request::Status { text } => {
                self.statuses.lock().unwrap().push(text);
                Ok(serde_json::Value::Null)
            }
            Request::EditorText => json(
                serde_json::to_value(EditorText {
                    path: Some("src/a.rs".into()),
                    language: Some("Rust".into()),
                    text: "hello".into(),
                    selection_start: 1,
                    selection_end: 3,
                })
                .unwrap(),
            ),
            Request::Edit { .. } => Ok(serde_json::Value::Null),
            Request::ReadFile { path } if path == "notes.txt" => json("from notes".into()),
            Request::ReadFile { .. } => Err("No such file".into()),
            Request::Http(http) => json(
                serde_json::to_value(HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: format!("got {}", http.url),
                })
                .unwrap(),
            ),
            Request::Log { .. } => unreachable!("the host keeps the log itself"),
        }
    }
}

impl Editor {
    fn last_status(&self) -> Option<String> {
        self.statuses.lock().unwrap().last().cloned()
    }
}

/// The probe with `permissions` and the commands the test runs.
fn manifest(permissions: &[&str], commands: &[&str]) -> Manifest {
    Manifest::parse(
        &serde_json::json!({
            "name": "probe",
            "permissions": permissions,
            "events": ["activate", "open", "change", "save"],
            "commands": commands.iter().map(|c| serde_json::json!({"id": c, "title": c})).collect::<Vec<_>>(),
        })
        .to_string(),
    )
    .unwrap()
}

const ALL: &[&str] = &[
    "statusBar",
    "editor:read",
    "editor:write",
    "fs:read",
    "http:127.0.0.1",
];

fn start(manifest: Manifest, budget: Budget) -> (Plugin, Arc<Editor>, Arc<Activity>) {
    let editor = Arc::new(Editor::default());
    let activity = Arc::new(Activity::default());
    let plugin = Plugin::start(
        manifest,
        testing::build(&testing::probe()),
        editor.clone(),
        activity.clone(),
        budget,
    );
    (plugin, editor, activity)
}

fn wait(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        // Generous: without optimizations the interpreter is some fifty
        // times slower, and CI builds it that way.
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn command(plugin: &Plugin, id: &str) {
    plugin.send(plugin::Event::Command { id: id.into() });
}

/// Runs `id` and returns the status it ends with.
fn run(plugin: &Plugin, editor: &Editor, id: &str) -> String {
    let before = editor.statuses.lock().unwrap().len();
    command(plugin, id);
    wait(id, || editor.statuses.lock().unwrap().len() > before);
    editor.last_status().unwrap()
}

#[test]
fn events_reach_the_plugin_and_its_requests_are_answered() {
    let commands = [
        "editor",
        "shout",
        "read:notes.txt",
        "read:missing.txt",
        "get:http://127.0.0.1:8080/x",
        "count",
        "log:hello from the probe",
    ];
    let (plugin, editor, _) = start(manifest(ALL, &commands), Budget::default());
    plugin.send(plugin::Event::Activate);
    wait("activate", || {
        editor.last_status().as_deref() == Some("active")
    });
    plugin.send(plugin::Event::Open {
        path: "src/a.rs".into(),
        language: None,
    });
    wait("open", || {
        editor.last_status().as_deref() == Some("open src/a.rs")
    });

    assert_eq!(run(&plugin, &editor, "editor"), "src/a.rs 1..3 5");
    assert_eq!(run(&plugin, &editor, "shout"), "edited");
    assert!(editor.requests.lock().unwrap().contains(&Request::Edit {
        path: "src/a.rs".into(),
        start: 0,
        end: 5,
        text: "HELLO".into(),
    }));
    assert_eq!(run(&plugin, &editor, "read:notes.txt"), "from notes");
    assert_eq!(
        run(&plugin, &editor, "read:missing.txt"),
        "refused: No such file"
    );
    assert_eq!(
        run(&plugin, &editor, "get:http://127.0.0.1:8080/x"),
        "200 got http://127.0.0.1:8080/x"
    );
    // One instance for all of them: its state lasts.
    assert_eq!(run(&plugin, &editor, "count"), "8 events");
    // A command the manifest does not list never reaches the plugin.
    command(&plugin, "undeclared");
    command(&plugin, "log:hello from the probe");
    wait("the log", || plugin.stats().log == ["hello from the probe"]);
    assert_eq!(run(&plugin, &editor, "count"), "10 events");
    let stats = plugin.stats();
    assert_eq!(
        (stats.failures, stats.over_budget, stats.stopped),
        (0, 0, 0)
    );
    assert!(!stats.slow() && stats.error.is_none());
}

#[test]
fn requests_without_a_declared_permission_never_reach_the_editor() {
    let commands = [
        "editor",
        "read:notes.txt",
        "get:http://127.0.0.1/x",
        "get:http://example.com/x",
    ];
    let (plugin, editor, _) = start(
        manifest(&["statusBar", "http:127.0.0.1"], &commands),
        Budget::default(),
    );
    assert_eq!(
        run(&plugin, &editor, "editor"),
        "refused: The plugin did not declare the permission editor:read"
    );
    assert_eq!(
        run(&plugin, &editor, "read:notes.txt"),
        "refused: The plugin did not declare the permission fs:read"
    );
    assert_eq!(
        run(&plugin, &editor, "get:http://example.com/x"),
        "refused: The plugin did not declare the permission http:example.com"
    );
    assert_eq!(
        run(&plugin, &editor, "get:http://127.0.0.1/x"),
        "200 got http://127.0.0.1/x"
    );
    // Only the statuses and the one allowed request got through.
    let requests = editor.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| !matches!(r, Request::Status { .. }))
            .count(),
        1
    );
}

#[test]
fn work_started_by_typing_waits_for_a_pause_and_is_counted() {
    let budget = Budget {
        // The probe's change handler needs a few million.
        typing: 100_000,
        idle: Duration::from_millis(120),
        ..Budget::default()
    };
    let (plugin, editor, activity) = start(manifest(ALL, &[]), budget);
    let change = || plugin.send(plugin::Event::Change { path: "a".into() });

    // While the user types, the plugin is over budget and held back.
    let typing = Instant::now();
    activity.touch();
    change();
    while typing.elapsed() < Duration::from_millis(400) {
        activity.touch();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(editor.last_status(), None, "ran while the user was typing");
    assert_eq!(plugin.stats().over_budget, 1);
    // Typing stopped: it finishes.
    wait("the deferred change", || {
        editor.last_status().as_deref() == Some("change a")
    });
    assert!(!plugin.stats().slow());

    // A burst is one change per file; three times over budget marks it.
    for _ in 0..2 {
        let before = editor.statuses.lock().unwrap().len();
        change();
        wait("a change", || {
            editor.statuses.lock().unwrap().len() > before
        });
    }
    let stats = plugin.stats();
    assert_eq!(stats.over_budget, 3);
    assert!(stats.slow());
    assert_eq!(stats.failures, 0);
}

#[test]
fn an_event_past_the_limit_is_stopped_and_the_plugin_starts_afresh() {
    let budget = Budget {
        slice: 2_000_000,
        limit: 10_000_000,
        ..Budget::default()
    };
    let (plugin, editor, _) = start(
        manifest(
            ALL,
            &["count", "forever", "panic", "alloc:1000", "alloc:100000000"],
        ),
        budget,
    );
    assert_eq!(run(&plugin, &editor, "count"), "1 events");
    assert_eq!(run(&plugin, &editor, "count"), "2 events");
    command(&plugin, "forever");
    wait("the stop", || plugin.stats().stopped == 1);
    let stats = plugin.stats();
    assert!(stats.slow());
    assert!(
        stats
            .last_failure
            .as_deref()
            .unwrap()
            .contains("past the limit")
    );
    // A new instance: the count starts over.
    assert_eq!(run(&plugin, &editor, "count"), "1 events");

    // A panic ends that event only.
    command(&plugin, "panic");
    wait("the panic", || plugin.stats().failures == 2);
    assert_eq!(run(&plugin, &editor, "count"), "1 events");

    // Memory is capped at 64 MiB.
    assert_eq!(run(&plugin, &editor, "alloc:1000"), "allocated 1000");
    command(&plugin, "alloc:100000000");
    wait("the failed allocation", || plugin.stats().failures == 3);
    assert_eq!(plugin.stats().error, None);
}

#[test]
fn a_module_that_is_not_a_plugin_says_so() {
    let host = Arc::new(Editor::default());
    let load = |wasm: &[u8]| {
        let plugin = Plugin::start(
            manifest(ALL, &[]),
            wasm.to_vec(),
            host.clone(),
            Arc::new(Activity::default()),
            Budget::default(),
        );
        wait("the load", || plugin.stats().error.is_some());
        plugin.stats().error.unwrap()
    };
    assert!(load(b"not wasm").starts_with("Could not load"));
    // The empty module: valid, but with nothing to call.
    assert_eq!(
        load(b"\0asm\x01\0\0\0"),
        "Could not load: The module has no solder_event function"
    );
    assert_eq!(Stats::default().error, None);
}

/// Not a check: prints what a unit of fuel is worth here, for `Budget`.
#[test]
fn fuel_per_millisecond() {
    let (plugin, editor, _) = start(
        manifest(ALL, &["spin:500000"]),
        Budget {
            slice: u64::MAX / 4,
            limit: u64::MAX / 2,
            ..Budget::default()
        },
    );
    let started = Instant::now();
    assert_eq!(run(&plugin, &editor, "spin:500000"), "spun");
    let ms = started.elapsed().as_secs_f64() * 1000.;
    let fuel = plugin.stats().last_fuel;
    eprintln!(
        "500 000 rounds of the probe's loop: {ms:.1} ms, {fuel} fuel, {:.0} fuel per ms",
        fuel as f64 / ms
    );
}
