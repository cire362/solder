//! Runs the check of a release against Open VSX and writes the list of
//! what works. For a machine that is thrown away afterwards: it runs the
//! code of the extensions it installs.
//!
//! `extension-census [--count 50] [--extension ID@VERSION] [--out extensions.md] [--catalog URL]`

use std::{path::PathBuf, time::Duration};

use extension::{catalog, census};

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut count, mut out, mut base) = (
        50,
        PathBuf::from("extensions.md"),
        catalog::OPEN_VSX.to_string(),
    );
    let mut extensions = Vec::new();
    while let Some(arg) = args.next() {
        let value = args.next().unwrap_or_default();
        match arg.as_str() {
            "--count" => count = value.parse().unwrap_or(count),
            "--out" => out = PathBuf::from(value),
            "--catalog" => base = value,
            "--extension" => {
                let Some((id, version)) = value.split_once('@') else {
                    eprintln!("extension-census: use --extension namespace.name@version");
                    std::process::exit(2);
                };
                extensions.push((id.to_string(), version.to_string()));
            }
            other => {
                eprintln!("extension-census: no option {other}");
                std::process::exit(2);
            }
        }
    }
    let node = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("node"))
            .find(|node| node.is_file())
    });
    let Some(node) = node else {
        eprintln!("extension-census: Node.js is not on the PATH");
        std::process::exit(1);
    };
    let root = std::env::temp_dir().join(format!("solder-census-{}", std::process::id()));
    let plan = census::Plan {
        catalog: base,
        count,
        extensions,
        root: root.clone(),
        node: node.to_string_lossy().into_owned(),
        patience: Duration::from_secs(60),
    };
    let rows = census::run(&plan, |name| eprintln!("{name}"));
    let _ = std::fs::remove_dir_all(&root);
    match rows.map(|rows| std::fs::write(&out, census::report(&rows)).map_err(|e| e.to_string())) {
        Ok(Ok(())) => eprintln!("extension-census: wrote {}", out.display()),
        Ok(Err(error)) | Err(error) => {
            eprintln!("extension-census: {error}");
            std::process::exit(1);
        }
    }
}
