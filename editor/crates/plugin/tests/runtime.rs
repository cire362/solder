//! The host against a real plugin: `fixtures/probe`, built for wasm32.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use plugin::{
    Activity, Budget, Code, EditorText, Host, HttpResponse, Manifest, Plugin, Reply, Request,
    Stats, testing,
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
        Code::Module(testing::build(&testing::probe())),
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
        // The probe's change handler works for milliseconds.
        typing: Duration::from_micros(200),
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
        limit: Duration::from_millis(60),
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
            Code::Module(wasm.to_vec()),
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

// ------------------------------------------------------------- JavaScript

fn start_script(manifest: Manifest, budget: Budget) -> (Plugin, Arc<Editor>, Arc<Activity>) {
    let editor = Arc::new(Editor::default());
    let activity = Arc::new(Activity::default());
    let plugin = Plugin::start(
        manifest,
        Code::Script(testing::probe_script()),
        editor.clone(),
        activity.clone(),
        budget,
    );
    (plugin, editor, activity)
}

#[test]
fn a_script_gets_events_and_its_requests_are_answered() {
    let commands = [
        "editor",
        "read:notes.txt",
        "read:missing.txt",
        "get:http://127.0.0.1:8080/x",
        "count",
        "log:hello",
        "later",
        "escape",
        "throw",
    ];
    let (plugin, editor, _) = start_script(manifest(ALL, &commands), Budget::default());
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
    assert_eq!(run(&plugin, &editor, "read:notes.txt"), "from notes");
    assert_eq!(
        run(&plugin, &editor, "read:missing.txt"),
        "refused: No such file"
    );
    assert_eq!(
        run(&plugin, &editor, "get:http://127.0.0.1:8080/x"),
        "200 got http://127.0.0.1:8080/x"
    );
    // Headers went as pairs.
    assert!(editor.requests.lock().unwrap().iter().any(|r| matches!(
        r,
        Request::Http(http) if http.headers == [("x-probe".to_string(), "1".to_string())]
    )));
    // One engine for all of them: the script's state lasts.
    assert_eq!(run(&plugin, &editor, "count"), "7 events");
    // console.log is the plugin's log.
    command(&plugin, "log:hello");
    wait("the log", || plugin.stats().log == ["hello 2"]);
    // Promises run between events.
    assert_eq!(run(&plugin, &editor, "later"), "later 42");
    // The engine's own modules are not there to use.
    assert_eq!(
        run(&plugin, &editor, "escape"),
        "undefined undefined undefined function"
    );
    // A handler that throws fails that event; the script goes on.
    command(&plugin, "throw");
    wait("the throw", || plugin.stats().failures == 1);
    let stats = plugin.stats();
    // What was thrown, and where in plugin.js.
    let thrown = stats.last_failure.unwrap();
    assert!(
        thrown.starts_with("Error: probe asked to throw at throw (plugin.js:4"),
        "{thrown}"
    );
    assert_eq!(run(&plugin, &editor, "count"), "12 events");
    assert_eq!((plugin.stats().stopped, plugin.stats().error), (0, None));
}

/// An editor whose file has characters that take one, two and four bytes.
struct Accents(Mutex<Vec<Request>>);

impl Host for Accents {
    fn request(&self, _: &str, request: Request) -> Reply {
        self.0.lock().unwrap().push(request.clone());
        match request {
            Request::EditorText => Ok(serde_json::to_value(EditorText {
                path: Some("a.txt".into()),
                language: None,
                // Selected: the emoji, bytes 3 to 7.
                text: "aé😀b".into(),
                selection_start: 3,
                selection_end: 7,
            })
            .unwrap()),
            _ => Ok(serde_json::Value::Null),
        }
    }
}

#[test]
fn a_script_counts_in_its_own_units_and_the_editor_in_bytes() {
    let host = Arc::new(Accents(Mutex::new(Vec::new())));
    let plugin = Plugin::start(
        manifest(ALL, &["wrap", "blind"]),
        Code::Script(testing::probe_script()),
        host.clone(),
        Arc::new(Activity::default()),
        Budget::default(),
    );
    let statuses = || -> Vec<String> {
        host.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|r| match r {
                Request::Status { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    };
    // JavaScript sees the selection at 2..4 and slices the emoji out by it.
    command(&plugin, "wrap");
    wait("the edit", || statuses() == ["edited 😀"]);
    assert!(host.0.lock().unwrap().contains(&Request::Edit {
        path: "a.txt".into(),
        start: 3,
        end: 7,
        text: "[😀]".into(),
    }));
    // Positions mean nothing without the text they were counted in.
    command(&plugin, "blind");
    wait("the refusal", || statuses().len() == 2);
    assert!(statuses()[1].starts_with("refused: Call solder.editor() before"));
}

#[test]
fn a_script_is_held_to_the_same_permissions_and_budget() {
    let commands = ["editor", "get:http://example.com/x", "forever", "count"];
    // A limit an honest handler stays under even where the interpreter is
    // built without optimizations, fifty times slower.
    let budget = Budget {
        typing: Duration::from_micros(200),
        limit: Duration::from_secs(1),
        idle: Duration::from_millis(120),
    };
    let (plugin, editor, activity) = start_script(
        manifest(&["statusBar", "http:127.0.0.1"], &commands),
        budget,
    );
    assert_eq!(
        run(&plugin, &editor, "editor"),
        "refused: The plugin did not declare the permission editor:read"
    );
    assert_eq!(
        run(&plugin, &editor, "get:http://example.com/x"),
        "refused: The plugin did not declare the permission http:example.com"
    );
    assert!(
        editor
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| matches!(r, Request::Status { .. }))
    );

    // Typing: over the budget, held while the user types, done after.
    let typing = Instant::now();
    activity.touch();
    plugin.send(plugin::Event::Change { path: "a".into() });
    while typing.elapsed() < Duration::from_millis(400) {
        activity.touch();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(editor.last_status().as_deref(), Some("change a"));
    assert_eq!(plugin.stats().over_budget, 1);
    wait("the deferred change", || {
        editor.last_status().as_deref() == Some("change a")
    });

    // An endless handler is stopped, and the script starts afresh.
    assert_eq!(run(&plugin, &editor, "count"), "4 events");
    command(&plugin, "forever");
    wait("the stop", || plugin.stats().stopped == 1);
    assert!(plugin.stats().slow());
    assert_eq!(run(&plugin, &editor, "count"), "1 events");
}

#[test]
fn a_script_that_cannot_run_says_why() {
    let load = |source: &str| {
        let plugin = Plugin::start(
            manifest(ALL, &[]),
            Code::Script(source.into()),
            Arc::new(Editor::default()),
            Arc::new(Activity::default()),
            Budget::default(),
        );
        wait("the load", || plugin.stats().error.is_some());
        plugin.stats().error.unwrap()
    };
    let error = load("solder.on('open', () => {");
    assert!(
        error.starts_with("Could not load: The script failed: SyntaxError: "),
        "{error}"
    );
    // The line is the script's own, not counted with the prelude before it.
    let error = load("// one\n// two\nthrow new Error('at the top')");
    assert_eq!(
        error,
        "Could not load: The script failed: Error: at the top at <anonymous> (plugin.js:3:11)"
    );
}
