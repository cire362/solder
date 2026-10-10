//! Python uses the same DAP connection and session as every other debugger.
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn configuration(root: &Path, file: &Path) -> crate::debug_launch::LaunchConfig {
    let shown = file.strip_prefix(root).unwrap_or(file).display();
    crate::debug_launch::LaunchConfig {
        name: format!("python {shown}"),
        request: json!({ "type": "debugpy", "request": "launch", "name": format!("python {shown}"),
            "program": file, "cwd": root, "python": [crate::test_runner::python(root)],
            "console": "internalConsole", "redirectOutput": true, "justMyCode": true, "subProcess": false }),
        ..Default::default()
    }
}

pub fn launch(
    root: &Path,
    mut request: Value,
    configured: Option<PathBuf>,
) -> Result<(dap::Adapter, dap::Link, Value), String> {
    let chosen = configured.unwrap_or_else(|| {
        request["python"][0]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::test_runner::python(root))
    });
    let program = if chosen.is_absolute() {
        Some(chosen.clone())
    } else if chosen.components().count() > 1 {
        Some(root.join(&chosen))
    } else {
        chosen
            .to_str()
            .and_then(|name| crate::lsp_store::find_program(name, root))
    }
    .ok_or_else(|| {
        format!(
            "Python was not found: {}. Set python_path in settings",
            chosen.display()
        )
    })?;
    // A missing module should say how to fix it, rather than look like an
    // adapter which silently closed its connection. Nothing is installed.
    let mut probe = Command::new(&program)
        .args([
            "-c",
            "import importlib.util,sys;sys.exit(0 if importlib.util.find_spec('debugpy') else 1)",
        ])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", program.display()))?;
    let start = Instant::now();
    loop {
        match probe.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => {
                return Err(format!(
                    "debugpy is missing or could not be loaded in {}. Install it in this Python environment",
                    program.display()
                ));
            }
            Err(e) => {
                let _ = probe.kill();
                let _ = probe.wait();
                return Err(e.to_string());
            }
            _ if start.elapsed() > Duration::from_secs(10) => {
                let _ = probe.kill();
                let _ = probe.wait();
                return Err("Python did not answer while checking debugpy".into());
            }
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    request["python"] = json!([program]);
    let args = vec!["-m".to_string(), "debugpy.adapter".to_string()];
    let launch = dap::Launch {
        program: &program,
        args: &args,
        env: &[],
        cwd: root,
        listen: None,
        patience: Duration::from_secs(15),
    };
    let (adapter, link) = dap::Adapter::launch(&launch).map_err(|e| e.to_string())?;
    Ok((
        adapter,
        link.ok_or("Python's debug adapter has no connection")?,
        request,
    ))
}
