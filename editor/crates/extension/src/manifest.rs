//! What an extension's folder contains, and what of it Solder runs.

use std::path::{Path, PathBuf};

use import::{ThemeFile, jsonc, theme};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Origin {
    Zed,
    VsCode,
}

impl Origin {
    /// The folder its extensions are kept in.
    pub fn folder(self) -> &'static str {
        match self {
            Origin::Zed => "zed",
            Origin::VsCode => "vscode",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Origin::Zed => "Zed",
            Origin::VsCode => "VS Code",
        }
    }
}

/// A tree-sitter grammar compiled to WebAssembly, with its queries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grammar {
    /// Its name inside the module (`tree_sitter_<symbol>`).
    pub symbol: String,
    pub module: PathBuf,
    pub highlights: Option<PathBuf>,
    pub injections: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Language {
    pub name: String,
    /// File name endings without the dot, and whole file names.
    pub suffixes: Vec<String>,
    /// Other names it goes by: a code fence name, a VS Code language id.
    pub aliases: Vec<String>,
    pub line_comment: Option<String>,
    /// Missing for a VS Code language: its TextMate grammar is not read.
    pub grammar: Option<Grammar>,
}

/// A language server the extension knows how to get and start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub languages: Vec<String>,
    /// What the server calls a language, where it differs from the
    /// language's name: `("TSX", "typescriptreact")`.
    pub language_ids: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetFile {
    pub path: PathBuf,
    /// Lowercase language ids; empty means every language.
    pub languages: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Code {
    None,
    /// A WebAssembly component built against this version of Zed's API.
    Zed {
        api: String,
    },
    /// A Node program written against VS Code's API. It does not run here.
    Node,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Extension {
    pub origin: Origin,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub dir: PathBuf,
    pub themes: Vec<ThemeFile>,
    pub snippets: Vec<SnippetFile>,
    pub languages: Vec<Language>,
    pub servers: Vec<Server>,
    pub code: Code,
    /// The commands its manifest declares its code runs: a program and the
    /// arguments it may be given (`*` for any one, `**` for any that remain).
    pub commands: Vec<(String, Vec<String>)>,
    /// What it has that Solder does not run, in words for the user.
    pub missing: Vec<String>,
}

impl Extension {
    /// One line on what Solder takes from it.
    pub fn provides(&self) -> String {
        let count = |n: usize, one: &str, many: &str| match n {
            0 => None,
            1 => Some(format!("1 {one}")),
            n => Some(format!("{n} {many}")),
        };
        let highlighted = self
            .languages
            .iter()
            .filter(|l| l.grammar.is_some())
            .count();
        let parts: Vec<String> = [
            count(highlighted, "language", "languages"),
            count(self.themes.len(), "theme", "themes"),
            count(self.snippets.len(), "snippet file", "snippet files"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if parts.is_empty() {
            "Nothing Solder can use".into()
        } else {
            parts.join(", ")
        }
    }
}

/// Reads the extension in `dir`, whichever editor it was made for; blocking.
pub fn read(dir: &Path) -> Result<Extension, String> {
    if dir.join("extension.toml").is_file() || dir.join("extension.json").is_file() {
        read_zed(dir)
    } else if dir.join("package.json").is_file() {
        read_vscode(dir)
    } else {
        Err("No extension.toml or package.json in it".into())
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().trim().to_string()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// Files directly in `dir` with this ending, sorted.
fn files(dir: &Path, ending: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == ending))
        .collect();
    found.sort();
    found
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// A path the manifest names, kept only if it stays inside the extension.
fn inside(dir: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative.trim_start_matches("./"));
    let plain = relative
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    plain.then(|| dir.join(relative))
}

fn read_zed(dir: &Path) -> Result<Extension, String> {
    // The manifest is TOML; extensions older than the format have JSON with
    // the same keys. Both become one JSON value.
    let manifest: Value = match std::fs::read_to_string(dir.join("extension.toml")) {
        Ok(source) => {
            toml::from_str(&source).map_err(|e| format!("extension.toml: {}", e.message()))?
        }
        Err(_) => std::fs::read_to_string(dir.join("extension.json"))
            .map_err(|e| e.to_string())
            .and_then(|source| jsonc::parse(&source))
            .map_err(|e| format!("extension.json: {e}"))?,
    };
    let id = text(&manifest["id"]);
    if id.is_empty() {
        return Err("The manifest has no id".into());
    }
    let mut missing = Vec::new();

    let themes = files(&dir.join("themes"), "json")
        .iter()
        .flat_map(|path| theme::all_zed(path))
        .collect();

    // Snippets: the file the manifest names, or one per language in
    // `snippets/`, where `snippets.json` is for every language.
    let mut snippet_paths = files(&dir.join("snippets"), "json");
    for named in strings(&manifest["snippets"])
        .into_iter()
        .chain(manifest["snippets"].as_str().map(str::to_string))
    {
        if let Some(path) = inside(dir, &named).filter(|p| p.is_file())
            && !snippet_paths.contains(&path)
        {
            snippet_paths.push(path);
        }
    }
    let snippets = snippet_paths
        .into_iter()
        .map(|path| {
            let language = stem(&path).to_lowercase();
            SnippetFile {
                languages: if language == "snippets" {
                    Vec::new()
                } else {
                    vec![language]
                },
                path,
            }
        })
        .collect();

    let mut languages = Vec::new();
    let mut folders: Vec<PathBuf> = std::fs::read_dir(dir.join("languages"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("config.toml").is_file())
        .collect();
    folders.sort();
    for folder in folders {
        let Some(config) = std::fs::read_to_string(folder.join("config.toml"))
            .ok()
            .and_then(|source| toml::from_str::<Value>(&source).ok())
        else {
            continue;
        };
        let name = text(&config["name"]);
        if name.is_empty() {
            continue;
        }
        let query = |file: &str| Some(folder.join(file)).filter(|p| p.is_file());
        let symbol = text(&config["grammar"]);
        let module = dir.join("grammars").join(format!("{symbol}.wasm"));
        let grammar = (!symbol.is_empty() && module.is_file()).then(|| Grammar {
            symbol,
            module,
            highlights: query("highlights.scm"),
            injections: query("injections.scm"),
        });
        languages.push(Language {
            suffixes: strings(&config["path_suffixes"]),
            aliases: config["code_fence_block_name"]
                .as_str()
                .map(str::to_string)
                .into_iter()
                .collect(),
            // Zed writes the space that follows the marker into it.
            line_comment: strings(&config["line_comments"])
                .first()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty()),
            grammar,
            name,
        });
    }

    let servers = manifest["language_servers"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(id, server)| {
            let mut languages = strings(&server["languages"]);
            if let Some(language) = server["language"].as_str() {
                languages.insert(0, language.to_string());
            }
            languages.dedup();
            let language_ids = server["language_ids"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(language, id)| Some((language.clone(), id.as_str()?.to_string())))
                .collect();
            Server {
                name: server["name"].as_str().unwrap_or(id).to_string(),
                id: id.clone(),
                languages,
                language_ids,
            }
        })
        .collect();

    let code = if dir.join("extension.wasm").is_file() {
        Code::Zed {
            api: text(&manifest["lib"]["version"]),
        }
    } else {
        Code::None
    };

    let commands = manifest["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|capability| capability["kind"] == "process:exec")
        .map(|capability| (text(&capability["command"]), strings(&capability["args"])))
        .collect();

    let has = |key: &str| match &manifest[key] {
        Value::Object(table) => !table.is_empty(),
        Value::Array(list) => !list.is_empty(),
        _ => false,
    };
    for (key, what) in [
        ("icon_themes", "Icon themes"),
        ("context_servers", "Context servers"),
        ("slash_commands", "Slash commands"),
        ("debug_adapters", "Debug adapters"),
        ("agent_servers", "Agent servers"),
        ("indexed_docs_providers", "Documentation indexing"),
    ] {
        if has(key) {
            missing.push(what.to_string());
        }
    }

    Ok(Extension {
        origin: Origin::Zed,
        name: Some(text(&manifest["name"]))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| id.clone()),
        id,
        version: text(&manifest["version"]),
        description: text(&manifest["description"]),
        dir: dir.to_path_buf(),
        themes,
        snippets,
        languages,
        servers,
        code,
        commands,
        missing,
    })
}

fn read_vscode(dir: &Path) -> Result<Extension, String> {
    let manifest = std::fs::read_to_string(dir.join("package.json"))
        .map_err(|e| e.to_string())
        .and_then(|source| jsonc::parse(&source))
        .map_err(|e| format!("package.json: {e}"))?;
    let (publisher, name) = (text(&manifest["publisher"]), text(&manifest["name"]));
    if publisher.is_empty() || name.is_empty() {
        return Err("package.json has no publisher or name".into());
    }
    // `%key%` stands for a string of package.nls.json.
    let nls = std::fs::read_to_string(dir.join("package.nls.json"))
        .ok()
        .and_then(|source| jsonc::parse(&source).ok())
        .unwrap_or(Value::Null);
    let label = |raw: &str| -> String {
        raw.strip_prefix('%')
            .and_then(|rest| rest.strip_suffix('%'))
            .and_then(|key| nls[key].as_str().or(nls[key]["message"].as_str()))
            .unwrap_or(raw)
            .to_string()
    };
    let contributes = &manifest["contributes"];
    let list = |key: &str| contributes[key].as_array().cloned().unwrap_or_default();
    let mut missing = Vec::new();

    let themes = theme::all_vscode(dir, &manifest, label);
    let declared = list("themes").len();
    if themes.len() < declared {
        missing.push(format!(
            "{} of its themes (not in the JSON format)",
            declared - themes.len()
        ));
    }

    let mut snippets: Vec<SnippetFile> = Vec::new();
    for entry in list("snippets") {
        let Some(path) = entry["path"]
            .as_str()
            .and_then(|p| inside(dir, p))
            .filter(|p| p.is_file())
        else {
            continue;
        };
        let language = text(&entry["language"]).to_lowercase();
        match snippets.iter_mut().find(|s| s.path == path) {
            // One file listed for several languages.
            Some(file) if !language.is_empty() => file.languages.push(language),
            Some(_) => {}
            None => snippets.push(SnippetFile {
                path,
                languages: Some(language)
                    .filter(|l| !l.is_empty())
                    .into_iter()
                    .collect(),
            }),
        }
    }

    // A language here is its file endings and its comment marker. The
    // grammar is TextMate's, which is not read.
    let mut languages = Vec::new();
    for entry in list("languages") {
        let id = text(&entry["id"]);
        let mut suffixes: Vec<String> = strings(&entry["extensions"])
            .iter()
            .map(|e| e.trim_start_matches('.').to_string())
            .collect();
        suffixes.extend(strings(&entry["filenames"]));
        if id.is_empty() || suffixes.is_empty() {
            continue;
        }
        let line_comment = entry["configuration"]
            .as_str()
            .and_then(|p| inside(dir, p))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|source| jsonc::parse(&source).ok())
            .and_then(|config| {
                config["comments"]["lineComment"]
                    .as_str()
                    .map(str::to_string)
            });
        let aliases = strings(&entry["aliases"]);
        languages.push(Language {
            name: aliases.first().cloned().unwrap_or_else(|| id.clone()),
            suffixes,
            aliases: std::iter::once(id).chain(aliases).collect(),
            line_comment,
            grammar: None,
        });
    }

    if !list("grammars").is_empty() {
        missing.push("Highlighting (a TextMate grammar)".into());
    }
    let code = if manifest["main"].is_string() || manifest["browser"].is_string() {
        missing.push("Its code, which needs VS Code".into());
        Code::Node
    } else {
        Code::None
    };
    for (key, what) in [
        ("iconThemes", "Icon themes"),
        ("productIconThemes", "Product icon themes"),
        ("debuggers", "Debuggers"),
        ("keybindings", "Key bindings for its commands"),
    ] {
        if !list(key).is_empty() {
            missing.push(what.to_string());
        }
    }

    let display = label(&text(&manifest["displayName"]));
    Ok(Extension {
        origin: Origin::VsCode,
        id: format!("{publisher}.{name}"),
        name: if display.is_empty() { name } else { display },
        version: text(&manifest["version"]),
        description: label(&text(&manifest["description"])),
        dir: dir.to_path_buf(),
        themes,
        snippets,
        languages,
        servers: Vec::new(),
        code,
        commands: Vec::new(),
        missing,
    })
}

/// The Zed extension that does what a VS Code extension does, for the ones
/// whose work is a language: there the Zed one brings a grammar and a
/// language server Solder can run.
pub fn zed_equivalent(vscode_id: &str) -> Option<&'static str> {
    let id = vscode_id.to_lowercase();
    EQUIVALENTS
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, zed)| *zed)
}

const EQUIVALENTS: &[(&str, &str)] = &[
    ("vue.volar", "vue"),
    ("prisma.prisma", "prisma"),
    ("svelte.svelte-vscode", "svelte"),
    ("astro-build.astro-vscode", "astro"),
    ("ms-azuretools.vscode-docker", "dockerfile"),
    ("ms-azuretools.vscode-containers", "dockerfile"),
    ("tamasfe.even-better-toml", "toml"),
    ("graphql.vscode-graphql", "graphql"),
    ("graphql.vscode-graphql-syntax", "graphql"),
    ("hashicorp.terraform", "terraform"),
    ("ms-dotnettools.csharp", "csharp"),
    ("redhat.java", "java"),
    ("shopify.ruby-lsp", "ruby"),
    ("bmewburn.vscode-intelephense-client", "php"),
    ("scala-lang.scala", "scala"),
    ("sumneko.lua", "lua"),
    ("ziglang.vscode-zig", "zig"),
    ("fwcd.kotlin", "kotlin"),
    ("haskell.haskell", "haskell"),
    ("ocamllabs.ocaml-platform", "ocaml"),
    ("elmtooling.elm-ls-vscode", "elm"),
    ("jakebecker.elixir-ls", "elixir"),
    ("dart-code.dart-code", "dart"),
    ("dart-code.flutter", "dart"),
    ("ecmel.vscode-html-css", "html"),
    ("twxs.cmake", "neocmake"),
    ("ms-vscode.cmake-tools", "neocmake"),
    ("zxh404.vscode-proto3", "proto"),
    ("redhat.vscode-xml", "xml"),
    ("mathiasfrohlich.kotlin", "kotlin"),
    ("nvarner.typst-lsp", "typst"),
    ("myriad-dreamin.tinymist", "typst"),
    ("james-yu.latex-workshop", "latex"),
    ("bbenoist.nix", "nix"),
    ("jnoortheen.nix-ide", "nix"),
    ("sswg.swift-lang", "swift"),
    ("swiftlang.swift-vscode", "swift"),
    ("mrmlnc.vscode-scss", "scss"),
    ("syler.sass-indented", "scss"),
];
