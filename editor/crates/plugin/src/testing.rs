//! Builds a plugin from source for tests, here and in the editor.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

/// The example plugin shipped with the editor.
pub fn word_count() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/word-count")
}

/// The example plugin written in TypeScript, with its compiled `plugin.js`.
pub fn word_count_ts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/word-count-ts")
}

/// The plugin that exercises every request.
pub fn probe() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/probe")
}

/// The script that exercises every request from JavaScript.
pub fn probe_script() -> String {
    std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/probe.js"),
    )
    .expect("probe.js")
}

/// Compiles the plugin project in `dir` for wasm32-unknown-unknown (once per
/// test run) and returns its module's bytes.
pub fn build(dir: &Path) -> Vec<u8> {
    static BUILT: Mutex<Option<HashMap<PathBuf, Vec<u8>>>> = Mutex::new(None);
    let mut built = BUILT.lock().unwrap_or_else(|e| e.into_inner());
    let built = built.get_or_insert_with(HashMap::new);
    if let Some(wasm) = built.get(dir) {
        return wasm.clone();
    }
    // Beside the workspace's own build, so `cargo clean` covers it.
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"))
        .join("wasm-plugins");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "build",
            "--release",
            "--locked",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .arg("--manifest-path")
        .arg(dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        // The outer build's flags are for the host's target.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .output()
        .expect("cargo runs");
    assert!(
        output.status.success(),
        "building {} failed (is the wasm32-unknown-unknown target installed?):\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).expect("Cargo.toml");
    let package = manifest
        .lines()
        .find_map(|l| l.strip_prefix("name = "))
        .expect("a package name")
        .trim_matches('"')
        .replace('-', "_");
    let wasm = std::fs::read(
        target
            .join("wasm32-unknown-unknown/release")
            .join(format!("{package}.wasm")),
    )
    .expect("the built module");
    built.insert(dir.to_path_buf(), wasm.clone());
    wasm
}

/// Builds the plugin in `dir` and puts it under `plugins`, as a user would
/// install it: a folder with `plugin.json` and `plugin.wasm`.
pub fn install(dir: &Path, plugins: &Path) -> PathBuf {
    let wasm = build(dir);
    let manifest = crate::Manifest::load(dir).expect("the fixture's manifest");
    let dest = plugins.join(&manifest.name);
    std::fs::create_dir_all(&dest).expect("the plugin folder");
    std::fs::copy(dir.join(crate::MANIFEST), dest.join(crate::MANIFEST)).expect("plugin.json");
    std::fs::write(dest.join(crate::MODULE), wasm).expect("plugin.wasm");
    dest
}

/// Puts the script plugin in `dir` under `plugins`: a folder with
/// `plugin.json` and `plugin.js`.
pub fn install_script(dir: &Path, plugins: &Path) -> PathBuf {
    let manifest = crate::Manifest::load(dir).expect("the plugin's manifest");
    let dest = plugins.join(&manifest.name);
    std::fs::create_dir_all(&dest).expect("the plugin folder");
    for file in [crate::MANIFEST, crate::SCRIPT] {
        std::fs::copy(dir.join(file), dest.join(file)).expect(file);
    }
    dest
}
