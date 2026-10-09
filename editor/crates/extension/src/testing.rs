//! What the tests of this crate and of the app share: a local HTTP server
//! standing in for a catalog, and extensions to serve from it.

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

pub struct Served {
    status: u16,
    location: Option<String>,
    body: Vec<u8>,
}

impl Served {
    pub fn ok(body: Vec<u8>) -> Self {
        Served {
            status: 200,
            location: None,
            body,
        }
    }

    pub fn status(status: u16) -> Self {
        Served {
            status,
            location: None,
            body: Vec::new(),
        }
    }

    /// A redirect to another path of the same server.
    pub fn redirect(path: &str) -> Self {
        Served {
            status: 307,
            location: Some(path.to_string()),
            body: Vec::new(),
        }
    }
}

/// Serves `routes` (by path, without the query) and returns the server's
/// base URL and every request line's target, in order.
pub fn serve(routes: Vec<(&'static str, Served)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buffer[..n]),
                }
            }
            let request = String::from_utf8_lossy(&request).into_owned();
            let target = request
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let path = target.split('?').next().unwrap_or_default().to_string();
            seen.lock().unwrap().push(target);
            let missing = Served::status(404);
            let served = routes
                .iter()
                .find(|(route, _)| *route == path)
                .map_or(&missing, |(_, served)| served);
            let mut head = format!(
                "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                served.status,
                served.body.len()
            );
            if let Some(location) = &served.location {
                head.push_str(&format!("Location: {location}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&served.body);
        }
    });
    (base, requests)
}

pub fn block<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// A fresh folder for one test.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("solder-extension-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

pub const ZED_ICON_THEME: &str = r#"{"name":"Demo Icons","author":"a","themes":[
        {"name":"Demo Icons","appearance":"dark",
         "directory_icons":{"collapsed":"./icons/folder.svg","expanded":"./icons/folder-open.svg"},
         "file_icons":{"default":{"path":"./icons/file.svg"},"rust":{"path":"./icons/rust.svg"}}}
    ]}"#;

/// A small picture, different for each name.
pub fn svg(name: &str) -> String {
    let shade = 0x30 + name.bytes().fold(0u8, |sum, b| sum.wrapping_add(b)) % 0xc0;
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\" viewBox=\"0 0 16 16\"><rect x=\"2\" y=\"2\" width=\"12\" height=\"12\" rx=\"3\" fill=\"#{shade:02x}80c0\"/></svg>"
    )
}

/// An icon theme as a VS Code extension writes one.
pub const VSCODE_ICON_THEME: &str = r#"{
  // Pictures are named once, then given to files.
  "iconDefinitions": {
    "_file": { "iconPath": "./../icons/file.svg" },
    "_folder": { "iconPath": "../icons/folder.svg" },
    "_folder_open": { "iconPath": "../icons/folder-open.svg" },
    "_src": { "iconPath": "../icons/src.svg" },
    "_src_open": { "iconPath": "../icons/src-open.svg" },
    "rust": { "iconPath": "../icons/rust.svg" },
    "_ts": { "iconPath": "../icons/ts.svg" },
    "_docker": { "iconPath": "../icons/docker.svg" },
    "_test": { "iconPath": "../icons/test.svg" },
    "_light": { "iconPath": "../icons/light.svg" },
    "_font": { "fontCharacter": "\\E001" },
    "_outside": { "iconPath": "../../outside.svg" }
  },
  "file": "_file",
  "folder": "_folder",
  "folderExpanded": "_folder_open",
  "folderNames": { "src": "_src", "docs": "_missing" },
  "folderNamesExpanded": { "src": "_src_open" },
  "fileExtensions": { "test.ts": "_test", "lock": "_outside", "woff": "_font" },
  "fileNames": { "dockerfile": "_docker" },
  "languageIds": { "rust": "rust", "typescript": "_ts", "typescriptreact": "_ts" },
  "light": { "file": "_light", "fileNames": { "dockerfile": "_light" } }
}"#;

pub const ZED_THEME: &str = r##"{"name":"Demo","author":"a","themes":[
        {"name":"Demo Dark","appearance":"dark","style":{"background":"#101014","editor.background":"#101014","text":"#e0e0e6","syntax":{"keyword":{"color":"#ff8800"}}}},
        {"name":"Demo Light","appearance":"light","style":{"background":"#fafafa","editor.background":"#fafafa","text":"#202020","syntax":{}}}
    ]}"##;

/// A Zed extension with one of everything Solder takes, in `dir`.
pub fn zed_extension(dir: &Path) {
    write(
        &dir.join("extension.toml"),
        r#"id = "demo"
name = "Demo"
version = "1.2.0"
schema_version = 1
description = "Demo language."
snippets = "./extra/all.json"
icon_themes = ["icon_themes/demo.json"]
capabilities = [
    { kind = "process:exec", command = "demo-ls", args = ["--version"] },
    { kind = "download_file", host = "example.com", path = ["**"] },
]

[lib]
kind = "Rust"
version = "0.7.0"

[grammars.demo]
repository = "https://example.com/tree-sitter-demo"
rev = "abc"

[language_servers.demo-ls]
name = "Demo LS"
language = "Demo"
languages = ["Demo", "Demo Template"]

[language_servers.demo-ls.language_ids]
"Demo Template" = "demo-template"
"#,
    );
    write(&dir.join("extension.wasm"), "component");
    write(&dir.join("grammars/demo.wasm"), "grammar");
    write(
        &dir.join("languages/demo/config.toml"),
        r#"name = "Demo"
grammar = "demo"
path_suffixes = ["demo", "Demofile"]
line_comments = ["// ", "/// "]
code_fence_block_name = "dm"
brackets = [
    { start = "{", end = "}", close = true, newline = true },
    { start = "\"", end = "\"", close = true, newline = false, not_in = ["string"] },
    { start = "", end = "x", close = true, newline = false },
]
autoclose_before = ";:.,=}])>"
block_comment = ["/* ", " */"]
word_characters = ["-", "$"]
completion_query_characters = ["-"]
increase_indent_pattern = '^.*\{\s*$'
decrease_indent_patterns = [
    { pattern = '^\s*\}', valid_after = ["if"] },
    { pattern = '^\s*end\b' },
]
"#,
    );
    write(
        &dir.join("languages/demo/highlights.scm"),
        "(comment) @comment",
    );
    write(&dir.join("languages/demo/indents.scm"), "(block) @indent");
    write(
        &dir.join("languages/demo/overrides.scm"),
        "(string) @string",
    );
    write(&dir.join("languages/demo/injections.scm"), "");
    write(
        &dir.join("languages/template/config.toml"),
        "name = \"Demo Template\"\ngrammar = \"missing\"\npath_suffixes = [\"dt\"]\n",
    );
    write(&dir.join("themes/demo.json"), ZED_THEME);
    write(&dir.join("icon_themes/demo.json"), ZED_ICON_THEME);
    for icon in ["file", "rust", "folder", "folder-open"] {
        write(&dir.join(format!("icons/{icon}.svg")), &svg(icon));
    }
    write(
        &dir.join("snippets/javascript.json"),
        r#"{"Log": {"prefix": "clg", "body": "console.log($1)"}}"#,
    );
    write(&dir.join("extra/all.json"), "{}");
}

/// A VS Code extension with themes, snippets, a language and code.
pub fn vscode_extension(dir: &Path) {
    write(
        &dir.join("package.json"),
        r#"{
  "name": "demo", "publisher": "Acme", "displayName": "%title%", "version": "3.0.1",
  "description": "Demo for VS Code", "main": "./out/main.js",
  "contributes": {
    "themes": [
      {"label": "Acme Dark", "uiTheme": "vs-dark", "path": "./themes/dark.json"},
      {"label": "Acme Old", "uiTheme": "vs-dark", "path": "./themes/old.tmTheme"},
      {"label": "Acme Broken", "uiTheme": "vs-dark", "path": "./themes/broken.tmTheme"}
    ],
    "snippets": [
      {"language": "javascript", "path": "./snippets/js.json"},
      {"language": "typescriptreact", "path": "./snippets/js.json"},
      {"language": "vue", "path": "../outside.json"}
    ],
    "languages": [
      {"id": "demo", "aliases": ["Demo Lang"], "extensions": [".dm"], "filenames": ["Demofile"], "configuration": "./language.json"}
    ],
    "grammars": [{"language": "demo", "scopeName": "source.demo", "path": "./demo.tmLanguage.json"}],
    "iconThemes": [
      {"id": "acme-icons", "label": "%icons%", "path": "./dist/icons.json"},
      {"id": "acme-font", "label": "Acme Font", "path": "./dist/font.json"}
    ],
    "configuration": [
      {"title": "Acme", "properties": {
        "acme.lint.level": {"type": "number", "default": 2, "description": "%level%"},
        "acme.format": {"type": "boolean", "default": true, "markdownDescription": "Formats on save.\nAnd more."}
      }},
      {"properties": {"acme.name": {"type": "string"}, "acme.format": {"default": false}}}
    ],
    "keybindings": [{"command": "demo.run", "key": "ctrl+r"}]
  }
}"#,
    );
    write(
        &dir.join("package.nls.json"),
        r#"{"title": "Acme Demo", "icons": "Acme Icons", "level": "How strict the linter is"}"#,
    );
    // Pictures beside the folder of the theme's file, as such themes
    // keep them.
    write(&dir.join("dist/icons.json"), VSCODE_ICON_THEME);
    for name in [
        "file",
        "folder",
        "folder-open",
        "src",
        "src-open",
        "rust",
        "ts",
        "docker",
        "test",
        "light",
    ] {
        write(&dir.join(format!("icons/{name}.svg")), &svg(name));
    }
    // One drawn with a font has no pictures to take.
    write(
        &dir.join("dist/font.json"),
        r#"{"fonts": [{"id": "f", "src": [{"path": "./f.woff", "format": "woff"}]}],
            "iconDefinitions": {"_file": {"fontCharacter": "\\E001"}}, "file": "_file"}"#,
    );
    write(
        &dir.join("themes/dark.json"),
        r##"{"name":"Acme Dark","type":"dark","colors":{"editor.background":"#101014","editor.foreground":"#e0e0e6"},"tokenColors":[{"scope":"keyword","settings":{"foreground":"#ff8800"}}]}"##,
    );
    // The format TextMate had: a property list.
    write(
        &dir.join("themes/old.tmTheme"),
        r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>name</key><string>Acme Old</string>
  <key>settings</key>
  <array>
    <dict><key>settings</key><dict>
      <key>background</key><string>#272822</string>
      <key>foreground</key><string>#F8F8F2</string>
      <key>selection</key><string>#49483E</string>
    </dict></dict>
    <dict>
      <key>name</key><string>Comment</string>
      <key>scope</key><string>comment</string>
      <key>settings</key><dict><key>foreground</key><string>#75715E</string></dict>
    </dict>
    <dict>
      <key>scope</key><string>keyword, storage</string>
      <key>settings</key><dict><key>foreground</key><string>#F92672</string></dict>
    </dict>
  </array>
</dict>
</plist>"##,
    );
    write(&dir.join("themes/broken.tmTheme"), "<plist/>");
    write(
        &dir.join("snippets/js.json"),
        r#"{"Log": {"prefix": "clg", "body": "console.log($1)"}}"#,
    );
    write(
        &dir.join("language.json"),
        r##"{ "comments": { "lineComment": "#", "blockComment": ["/*", "*/"] } } // trailing"##,
    );
    write(&dir.join("out/main.js"), "module.exports = {}");
}

pub fn tar(folder: &Path) -> Vec<u8> {
    let archive = folder.with_extension("tgz");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(folder)
        .arg(".")
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(archive).unwrap()
}

/// A zip of `folder`'s content, or `None` where python3 is missing.
pub fn zip(folder: &Path, inner: &str) -> Option<Vec<u8>> {
    let archive = folder.with_extension("vsix");
    let status = Command::new("python3")
        .current_dir(folder)
        .args(["-m", "zipfile", "-c"])
        .arg(&archive)
        .arg(inner)
        .status()
        .ok()?;
    assert!(status.success());
    Some(std::fs::read(archive).unwrap())
}
