//! Runs llama.cpp's server for one model, on a free local port with a key
//! made for this run, so other programs on the machine cannot use it.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use crate::client;

/// Loading a large model from a cold disk takes a while.
const LOAD_TIMEOUT: Duration = Duration::from_secs(300);

pub struct LocalServer {
    child: Child,
    pub port: u16,
    pub key: String,
    pub model: PathBuf,
    pub log: PathBuf,
}

impl LocalServer {
    /// Starts the server and waits until the model is loaded.
    pub async fn start(
        binary: PathBuf,
        model: PathBuf,
        context: u32,
        logs: PathBuf,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&logs).map_err(|e| e.to_string())?;
        stop_stale(&logs);
        let port = free_port()?;
        let key = random_key();
        let log = logs.join("llama-server.log");
        let out = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let mut command = Command::new(&binary);
        command
            .arg("--model")
            .arg(&model)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["--ctx-size", &context.to_string()])
            .args(["--api-key", &key, "--no-webui"])
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        // The build ships its libraries next to the binary.
        if let Some(dir) = binary.parent() {
            command.env("LD_LIBRARY_PATH", dir);
        }
        let child = command
            .spawn()
            .map_err(|e| format!("Could not start llama-server: {e}"))?;
        let _ = std::fs::write(logs.join("llama-server.pid"), child.id().to_string());
        watch(std::process::id(), child.id());
        let mut server = Self {
            child,
            port,
            key,
            model,
            log,
        };
        server.wait_ready().await?;
        Ok(server)
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    async fn wait_ready(&mut self) -> Result<(), String> {
        let start = Instant::now();
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!(
                    "llama-server stopped ({status}): {}",
                    log_tail(&self.log)
                ));
            }
            // 503 while the model loads, 200 once it can answer.
            if let Ok(response) = client()
                .get(format!("{}/health", self.url()))
                .timeout(Duration::from_secs(2))
                .send()
                .await
                && response.status().is_success()
            {
                return Ok(());
            }
            if start.elapsed() > LOAD_TIMEOUT {
                return Err("llama-server did not load the model in time".into());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(dir) = self.log.parent() {
            let _ = std::fs::remove_file(dir.join("llama-server.pid"));
        }
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Stops the server if the editor dies without stopping it (a crash or a
/// kill), so it does not keep gigabytes of memory. A detached shell polls
/// both processes; it only kills a process still named llama-server, in
/// case the id was reused. On Windows the stale check below covers it.
fn watch(editor: u32, server: u32) {
    if !cfg!(unix) {
        return;
    }
    let script = r#"(
        while kill -0 "$1" 2>/dev/null && kill -0 "$2" 2>/dev/null; do sleep 2; done
        if ! kill -0 "$1" 2>/dev/null && ps -p "$2" -o command= 2>/dev/null | grep -q llama-server; then
            kill "$2"
        fi
    ) >/dev/null 2>&1 &"#;
    // The outer shell exits at once, leaving the loop to init, so nothing
    // waits on it here.
    let _ = Command::new("sh")
        .args(["-c", script, "sh", &editor.to_string(), &server.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A server left by an editor that crashed still holds its memory.
fn stop_stale(logs: &Path) {
    let Ok(pid) = std::fs::read_to_string(logs.join("llama-server.pid")) else {
        return;
    };
    let pid = pid.trim();
    if cfg!(unix)
        && pid.bytes().all(|b| b.is_ascii_digit())
        && let Ok(out) = Command::new("ps").args(["-p", pid, "-o", "comm="]).output()
        && String::from_utf8_lossy(&out.stdout).contains("llama-server")
    {
        let _ = Command::new("kill").arg(pid).status();
    }
    let _ = std::fs::remove_file(logs.join("llama-server.pid"));
}

fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    Ok(listener.local_addr().map_err(|e| e.to_string())?.port())
}

fn random_key() -> String {
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes));
    if read.is_err() {
        // No /dev/urandom (Windows): hash the process's random seed.
        use std::hash::{BuildHasher, Hasher};
        for chunk in bytes.chunks_mut(8) {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(Instant::now().elapsed().as_nanos());
            chunk.copy_from_slice(&h.finish().to_le_bytes()[..chunk.len()]);
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The last lines of the server's log: why it stopped.
fn log_tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" / ")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_server_outlives_no_dead_editor() {
        // A stand-in editor (`sleep`) and server: a script named like the
        // real one, so the name check passes. (A copy of a system binary
        // would be killed by macOS.)
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("solder-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("llama-server");
        std::fs::write(&fake, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut editor = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let mut server = Command::new(&fake).arg("30").spawn().unwrap();
        watch(editor.id(), server.id());
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            server.try_wait().unwrap().is_none(),
            "alive while the editor is"
        );
        editor.kill().unwrap();
        editor.wait().unwrap();
        let start = Instant::now();
        while server.try_wait().unwrap().is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the server outlived the editor"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
