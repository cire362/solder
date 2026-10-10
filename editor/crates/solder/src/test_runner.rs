//! Explicit discovery and runs, outside the UI thread. Runners supply the
//! test names; source lookup is only for a jump, never for choosing a test.

use serde_json::Value;
use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const LIMIT: usize = 2 * 1024 * 1024;
const PYTHON: &str = include_str!("test_unittest.py");

#[derive(Clone, Debug)]
pub enum Runner {
    Rust {
        manifest: PathBuf,
        target: String,
        kind: String,
        package: String,
    },
    Python {
        program: PathBuf,
    },
    Go {
        package: String,
    },
}

#[derive(Clone, Debug)]
pub struct Test {
    pub group: String,
    pub name: String,
    pub path: Option<PathBuf>,
    pub line: u32,
    pub runner: Runner,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum State {
    #[default]
    Ready,
    Running,
    Passed,
    Failed,
    Skipped,
    Cancelled,
}
impl State {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready => "",
            Self::Running => "Running",
            Self::Passed => "Passed",
            Self::Failed => "Failed",
            Self::Skipped => "Skipped",
            Self::Cancelled => "Cancelled",
        }
    }
}

#[derive(Default)]
pub struct Discovery {
    pub tests: Vec<Test>,
    pub errors: Vec<String>,
}
pub struct Outcome {
    pub state: State,
    pub output: String,
}
pub type Cancel = Arc<AtomicBool>;

struct Output {
    ok: bool,
    text: String,
    stdout: String,
    stderr: String,
    limited: bool,
}

fn output(mut command: Command, cancel: &Cancel) -> Result<Output, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|mut stream| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut limited = false;
            let mut buf = [0; 8192];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let keep = n.min((LIMIT / 2).saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buf[..keep]);
                limited |= keep < n;
            }
            (bytes, limited)
        })
    })
    .collect();
    let start = Instant::now();
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err("Cancelled".into());
        }
        if start.elapsed() > Duration::from_secs(600) {
            break Err("The test command timed out after 10 minutes".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.success()),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(e.to_string()),
        }
    };
    // A test can leave a child holding its output pipe open after it ends.
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-9", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
    let streams: Vec<_> = readers
        .into_iter()
        .map(|r| r.join().unwrap_or_default())
        .collect();
    let stdout = String::from_utf8_lossy(&streams[0].0).into_owned();
    let stderr = String::from_utf8_lossy(&streams[1].0).into_owned();
    let mut text = format!("{stdout}\n{stderr}");
    let limited = streams.iter().any(|s| s.1);
    if limited {
        text.push_str("\nOutput limited to 1 MB per stream.\n");
    }
    result.map(|ok| Output {
        ok,
        text,
        stdout,
        stderr,
        limited,
    })
}

fn discovery_output(command: Command, cancel: &Cancel) -> Result<Output, String> {
    let out = output(command, cancel)?;
    if out.limited {
        return Err("Test discovery exceeded 1 MB per stream; its list is incomplete".into());
    }
    if !out.ok {
        return Err(out.text);
    }
    Ok(out)
}

fn command(program: impl AsRef<std::ffi::OsStr>, root: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .env("PATH", crate::agent::search_path());
    #[cfg(test)]
    command.env("CARGO_TARGET_DIR", root.join("target"));
    command
}

fn lines<'a>(text: &'a str, prefix: &str) -> impl Iterator<Item = Value> + 'a {
    let prefix = prefix.to_string();
    text.lines()
        .filter_map(move |line| serde_json::from_str(line.strip_prefix(&prefix)?).ok())
}

pub fn python(root: &Path) -> PathBuf {
    for path in [
        root.join(".venv/bin/python"),
        root.join("venv/bin/python"),
        root.join(".venv/Scripts/python.exe"),
        root.join("venv/Scripts/python.exe"),
    ] {
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from(if cfg!(windows) { "python" } else { "python3" })
}

pub fn discover(root: &Path, cancel: &Cancel) -> Discovery {
    // Runners report canonical paths, including macOS /private/var.
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root = canonical.as_path();
    let mut found = Discovery::default();
    if root.join("Cargo.toml").is_file() {
        let mut cmd = command("cargo", root);
        cmd.args(["test", "--workspace", "--no-run", "--message-format=json"]);
        let rust = (|| {
            let built = discovery_output(cmd, cancel)?;
            if !built.ok {
                return Err(built.text);
            }
            let sources = source_locations(root, "rs");
            let mut seen = HashSet::new();
            for artifact in lines(&built.stdout, "") {
                if artifact["reason"] != "compiler-artifact" || artifact["profile"]["test"] != true
                {
                    continue;
                }
                let Some(exe) = artifact["executable"].as_str() else {
                    continue;
                };
                if !seen.insert(exe.to_string()) {
                    continue;
                }
                let mut cmd = command(exe, root);
                cmd.args(["--list", "--format", "terse"]);
                let listed = discovery_output(cmd, cancel)?;
                if !listed.ok {
                    return Err(listed.text);
                }
                let target = artifact["target"]["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let kind = artifact["target"]["kind"][0]
                    .as_str()
                    .unwrap_or("lib")
                    .to_string();
                let manifest =
                    PathBuf::from(artifact["manifest_path"].as_str().unwrap_or("Cargo.toml"));
                let folder = manifest
                    .parent()
                    .unwrap_or(root)
                    .strip_prefix(root)
                    .unwrap_or(root);
                let group = if folder.as_os_str().is_empty() {
                    format!("Rust · {target} ({kind})")
                } else {
                    format!("Rust · {} · {target} ({kind})", folder.display())
                };
                for name in listed
                    .stdout
                    .lines()
                    .filter_map(|line| line.strip_suffix(": test"))
                {
                    let (path, line) = location(
                        &sources,
                        name.rsplit("::").next().unwrap_or(name),
                        manifest.parent().unwrap_or(root),
                    );
                    found.tests.push(Test {
                        group: group.clone(),
                        name: name.into(),
                        path,
                        line,
                        runner: Runner::Rust {
                            manifest: manifest.clone(),
                            target: target.clone(),
                            kind: kind.clone(),
                            package: artifact["package_id"].as_str().unwrap_or("").into(),
                        },
                    });
                }
            }
            Ok::<_, String>(())
        })();
        if let Err(e) = rust {
            found.errors.push(format!("Rust: {e}"));
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return found;
    }
    let files = crate::project::scan(root);
    if files
        .iter()
        .any(|p| p.ends_with(".py") && (p.contains("test_") || p.ends_with("_test.py")))
    {
        let program = python(root);
        let mut cmd = command(&program, root);
        cmd.args(["-c", PYTHON, "list"]);
        match discovery_output(cmd, cancel) {
            Ok(out) if out.ok => {
                for row in lines(&out.stdout, "solder-test:") {
                    let Some(name) = row["id"].as_str() else {
                        continue;
                    };
                    found.tests.push(Test {
                        group: "Python · unittest".into(),
                        name: name.into(),
                        path: row["path"].as_str().map(PathBuf::from),
                        line: row["line"].as_u64().unwrap_or(1) as u32,
                        runner: Runner::Python {
                            program: program.clone(),
                        },
                    });
                }
            }
            Ok(out) => found.errors.push(format!("Python: {}", out.text)),
            Err(e) => found.errors.push(format!("Python: {e}")),
        }
    }
    if root.join("go.mod").is_file() && !cancel.load(Ordering::Relaxed) {
        let mut cmd = command("go", root);
        cmd.args(["list", "-f", "{{.ImportPath}}|{{.Dir}}", "./..."]);
        let go = (|| {
            let packages = discovery_output(cmd, cancel)?;
            if !packages.ok {
                return Err(packages.text);
            }
            let sources = source_locations(root, "go");
            for item in packages.stdout.lines().filter(|s| !s.is_empty()) {
                let Some((package, dir)) = item.split_once('|') else {
                    continue;
                };
                let mut cmd = command("go", root);
                cmd.args(["test", "-list", ".", package]);
                let listed = discovery_output(cmd, cancel)?;
                if !listed.ok {
                    return Err(listed.text);
                }
                for name in listed
                    .stdout
                    .lines()
                    .filter(|s| s.starts_with("Test") || s.starts_with("Example"))
                {
                    let (path, line) = location(&sources, name, Path::new(dir));
                    found.tests.push(Test {
                        group: format!("Go · {package}"),
                        name: name.into(),
                        path,
                        line,
                        runner: Runner::Go {
                            package: package.into(),
                        },
                    });
                }
            }
            Ok::<_, String>(())
        })();
        if let Err(e) = go {
            found.errors.push(format!("Go: {e}"));
        }
    }
    found
        .tests
        .sort_by(|a, b| (&a.group, &a.name).cmp(&(&b.group, &b.name)));
    found
}

fn source_locations(root: &Path, ext: &str) -> Vec<(String, PathBuf, u32)> {
    let pattern = if ext == "rs" {
        r"\bfn\s+([A-Za-z_][A-Za-z_0-9]*)\s*\("
    } else {
        r"\bfunc\s+([A-Za-z_][A-Za-z_0-9]*)\s*\("
    };
    let regex = regex::Regex::new(pattern).unwrap();
    let mut out = Vec::new();
    for file in crate::project::scan(root)
        .iter()
        .filter(|file| file.ends_with(&format!(".{ext}")))
    {
        let path = root.join(file.as_ref());
        let Ok(loaded) = crate::document::load(&path) else {
            continue;
        };
        for (line, text) in loaded.text.lines().enumerate() {
            for capture in regex.captures_iter(text) {
                out.push((capture[1].into(), path.clone(), line as u32 + 1));
            }
        }
    }
    out
}

fn location(sources: &[(String, PathBuf, u32)], name: &str, dir: &Path) -> (Option<PathBuf>, u32) {
    let matches: Vec<_> = sources
        .iter()
        .filter(|(n, p, _)| n == name && p.starts_with(dir))
        .collect();
    match matches.as_slice() {
        [(_, path, line)] => (Some(path.clone()), *line),
        _ => (None, 1),
    }
}

pub fn run(root: &Path, test: &Test, cancel: &Cancel) -> Outcome {
    let mut cmd = match &test.runner {
        Runner::Rust {
            manifest,
            target,
            kind,
            package,
        } => {
            let mut cmd = command("cargo", root);
            cmd.arg("test")
                .arg("--manifest-path")
                .arg(manifest)
                .arg("-p")
                .arg(package);
            match kind.as_str() {
                "lib" | "rlib" | "proc-macro" => {
                    cmd.arg("--lib");
                }
                "bin" => {
                    cmd.arg("--bin").arg(target);
                }
                _ => {
                    cmd.arg("--test").arg(target);
                }
            }
            cmd.arg(&test.name)
                .args(["--", "--exact", "--nocapture", "--color", "never"]);
            cmd
        }
        Runner::Python { program } => {
            let mut cmd = command(program, root);
            cmd.args(["-c", PYTHON, "run", &test.name]);
            cmd
        }
        Runner::Go { package } => {
            let mut cmd = command("go", root);
            cmd.args([
                "test",
                "-count=1",
                "-json",
                "-run",
                &format!("^{}$", regex::escape(&test.name)),
                package,
            ]);
            cmd
        }
    };
    cmd.env("NO_COLOR", "1");
    match output(cmd, cancel) {
        Ok(out) => {
            let skipped = match &test.runner {
                Runner::Python { .. } => lines(&out.stdout, "solder-test:")
                    .any(|v| v["id"] == test.name && v["state"] == "skipped"),
                Runner::Rust { .. } => out
                    .text
                    .contains(&format!("test {} ... ignored", test.name)),
                Runner::Go { .. } => {
                    lines(&out.stdout, "").any(|v| v["Test"] == test.name && v["Action"] == "skip")
                }
            };
            let ran = match &test.runner {
                Runner::Rust { .. } => out
                    .text
                    .lines()
                    .any(|s| s.starts_with(&format!("test {} ...", test.name))),
                Runner::Python { .. } => {
                    lines(&out.stdout, "solder-test:").any(|v| v["id"] == test.name)
                }
                Runner::Go { .. } => lines(&out.stdout, "").any(|v| {
                    v["Test"] == test.name
                        && matches!(v["Action"].as_str(), Some("pass" | "fail" | "skip"))
                }),
            };
            let state = if !out.ok || !ran {
                State::Failed
            } else if skipped {
                State::Skipped
            } else {
                State::Passed
            };
            // Python reserves stdout for its result protocol and routes
            // program output to stderr. Only the latter belongs on screen.
            let displayed = if matches!(test.runner, Runner::Python { .. }) {
                format!(
                    "{}{}",
                    out.stderr,
                    if out.limited {
                        "\nOutput limited to 1 MB per stream.\n"
                    } else {
                        ""
                    }
                )
            } else {
                out.text
            };
            let output = if !ran && out.ok {
                format!(
                    "The selected test did not run. Discover tests again.\n{}",
                    displayed
                )
            } else {
                displayed
            };
            Outcome { state, output }
        }
        Err(output) => Outcome {
            state: if cancel.load(Ordering::Relaxed) {
                State::Cancelled
            } else {
                State::Failed
            },
            output,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rust_lists_real_tests_and_keeps_exact_runs_and_ignored_results() {
        if Command::new("cargo").arg("--version").output().is_err() {
            return;
        }
        let root = db::testing::dir("test-runner-rust").canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='tiny'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "#[test]\nfn good() {}\n#[test]\nfn bad() { panic!(\"a clear failure\"); }\n#[test]\n#[ignore]\nfn later() {}\n").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let found = discover(&root, &cancel);
        assert!(found.errors.is_empty(), "{:?}", found.errors);
        assert_eq!(found.tests.len(), 3);
        let test = |name| found.tests.iter().find(|t| t.name == name).unwrap();
        assert_eq!(test("bad").path, Some(root.join("src/lib.rs")));
        assert_eq!(test("bad").line, 4);
        assert_eq!(run(&root, test("good"), &cancel).state, State::Passed);
        let bad = run(&root, test("bad"), &cancel);
        assert_eq!(bad.state, State::Failed);
        assert!(bad.output.contains("a clear failure"));
        assert!(!bad.output.contains("test good ..."));
        assert_eq!(run(&root, test("later"), &cancel).state, State::Skipped);
        std::fs::write(root.join("src/lib.rs"), "#[test]\nfn different() {}\n").unwrap();
        assert_eq!(run(&root, test("good"), &cancel).state, State::Failed);
    }
    #[test]
    fn go_lists_real_tests_and_reports_failures() {
        if Command::new("go").arg("version").output().is_err() {
            return;
        }
        let root = db::testing::dir("test-runner-go");
        std::fs::write(root.join("go.mod"), "module example.com/tiny\ngo 1.21\n").unwrap();
        std::fs::write(root.join("tiny_test.go"),"package tiny\nimport \"testing\"\nfunc TestGood(t *testing.T) {}\nfunc TestBad(t *testing.T) { t.Fatal(\"a clear failure\") }\n").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let found = discover(&root, &cancel);
        assert!(found.errors.is_empty(), "{:?}", found.errors);
        assert_eq!(found.tests.len(), 2);
        for test in &found.tests {
            let result = run(&root, test, &cancel);
            assert_eq!(
                result.state,
                if test.name == "TestGood" {
                    State::Passed
                } else {
                    State::Failed
                }
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn a_running_process_is_cancelled_without_waiting_for_its_timeout() {
        let root = db::testing::dir("test-runner-cancel");
        let cancel = Arc::new(AtomicBool::new(false));
        let stopped = cancel.clone();
        let mut cmd = command("/bin/sh", &root);
        cmd.args(["-c", "sleep 30"]);
        let job = std::thread::spawn(move || output(cmd, &stopped));
        std::thread::sleep(Duration::from_millis(100));
        cancel.store(true, Ordering::Relaxed);
        let start = Instant::now();
        assert_eq!(job.join().unwrap().err().as_deref(), Some("Cancelled"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn an_oversized_discovery_never_looks_like_a_complete_test_list() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let root = db::testing::dir("test-list-limit");
        let mut cmd = command("python3", &root);
        cmd.args(["-c", "import sys; sys.stdout.write('x' * 1100000)"]);
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(
            discovery_output(cmd, &cancel)
                .err()
                .unwrap()
                .contains("list is incomplete")
        );
    }

    #[test]
    fn unittest_lists_locations_and_runs_exactly_one_test() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let root = db::testing::dir("test-runner").canonicalize().unwrap();
        std::fs::write(root.join("test_example.py"), "import unittest\nclass Tests(unittest.TestCase):\n    def test_pass(self): self.assertEqual(2, 2); print('solder-test: user output')\n    def test_fail(self): self.assertEqual(1, 2)\n    @unittest.skip('later')\n    def test_skip(self): pass\n    @unittest.expectedFailure\n    def test_expected(self): self.fail('known')\n    @unittest.expectedFailure\n    def test_unexpected(self): pass\n").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let found = discover(&root, &cancel);
        assert!(found.errors.is_empty(), "{:?}", found.errors);
        assert_eq!(found.tests.len(), 5);
        let test = |name: &str| found.tests.iter().find(|t| t.name.ends_with(name)).unwrap();
        assert_eq!(test("test_pass").line, 3);
        assert_eq!(test("test_pass").path, Some(root.join("test_example.py")));
        let passed = run(&root, test("test_pass"), &cancel);
        assert_eq!(passed.state, State::Passed);
        assert!(passed.output.contains("solder-test: user output"));
        let failed = run(&root, test("test_fail"), &cancel);
        assert_eq!(failed.state, State::Failed);
        assert!(failed.output.contains("AssertionError: 1 != 2"));
        assert!(!failed.output.contains("solder-test:"));
        assert!(!failed.output.contains("test_pass"));
        assert_eq!(run(&root, test("test_skip"), &cancel).state, State::Skipped);
        assert_eq!(
            run(&root, test("test_expected"), &cancel).state,
            State::Skipped
        );
        let unexpected = run(&root, test("test_unexpected"), &cancel);
        assert_eq!(unexpected.state, State::Failed);
        assert!(
            unexpected
                .output
                .to_lowercase()
                .contains("unexpected success")
        );
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            run(&root, test("test_pass"), &cancel).state,
            State::Cancelled
        );
    }
}
