//! What an extension's folder contains, and what of it Solder runs.

use std::path::{Path, PathBuf};

use import::{ThemeFile, jsonc, theme};

use crate::icons::{self, IconTheme};
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
    pub indents: Option<PathBuf>,
    pub brackets: Option<PathBuf>,
    pub outline: Option<PathBuf>,
    /// Gives names to the places `Pair::not_in` speaks of.
    pub overrides: Option<PathBuf>,
}

/// Two pieces of text that go together: brackets, quotes, a tag's ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pair {
    pub start: String,
    pub end: String,
    /// The end is typed for the user when the start is.
    pub close: bool,
    /// Enter between the two puts the end on a line of its own.
    pub newline: bool,
    /// Where it does not close: `string`, `comment`.
    pub not_in: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Language {
    pub name: String,
    /// File name endings without the dot, and whole file names.
    pub suffixes: Vec<String>,
    /// Other names it goes by: a code fence name, a VS Code language id.
    pub aliases: Vec<String>,
    pub line_comment: Option<String>,
    /// The start and the end of a comment that has both.
    pub block_comment: Option<(String, String)>,
    pub pairs: Vec<Pair>,
    /// What a pair may close in front of, besides a blank.
    pub autoclose_before: Option<String>,
    /// Characters that belong to a word besides letters, digits and `_`.
    pub word_characters: String,
    /// The same for the word a completion goes on from.
    pub completion_characters: String,
    /// Regular expressions: a line that matches the first is followed by a
    /// deeper one, a line that matches the second goes one level back.
    pub increase_indent: Option<String>,
    pub decrease_indent: Option<String>,
    /// The debug adapters that debug it, by name.
    pub debuggers: Vec<String>,
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
    /// Which picture goes with which file, in the tree and on tabs.
    pub icon_themes: Vec<IconTheme>,
    pub snippets: Vec<SnippetFile>,
    pub languages: Vec<Language>,
    pub servers: Vec<Server>,
    /// The debug adapters its code knows how to get and start, by name.
    pub debug_adapters: Vec<String>,
    /// The context servers (MCP servers) its code knows how to start.
    pub context_servers: Vec<String>,
    pub code: Code,
    /// The commands its manifest declares its code runs: a program and the
    /// arguments it may be given (`*` for any one, `**` for any that remain).
    pub commands: Vec<(String, Vec<String>)>,
    /// What it has that Solder does not run, in words for the user.
    pub missing: Vec<String>,
}

impl Extension {
    /// Whether its code runs in Solder's host.
    pub fn runs_code(&self) -> bool {
        matches!(&self.code, Code::Zed { api } if crate::host::runs(api))
    }

    /// What it does outside a sandbox once installed, each in a sentence
    /// for the user: the language servers its code downloads and starts,
    /// and the commands its manifest declares. Empty for an extension that
    /// is only data, and for code Solder does not run.
    pub fn outside(&self) -> Vec<String> {
        if !self.runs_code() {
            return Vec::new();
        }
        let servers = self
            .servers
            .iter()
            .map(|server| format!("Download and start the language server {}", server.name))
            .chain(
                self.debug_adapters
                    .iter()
                    .map(|adapter| format!("Get and start the debug adapter {adapter}")),
            )
            .chain(
                self.context_servers
                    .iter()
                    .map(|server| format!("Get and start the context server {server}")),
            );
        let commands = self.commands.iter().map(|(program, args)| {
            if args.is_empty() {
                format!("Run {program}")
            } else {
                format!("Run {program} {}", args.join(" "))
            }
        });
        servers.chain(commands).collect()
    }

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
            count(
                if self.runs_code() {
                    self.servers.len()
                } else {
                    0
                },
                "language server",
                "language servers",
            ),
            count(
                if self.runs_code() {
                    self.debug_adapters.len()
                } else {
                    0
                },
                "debug adapter",
                "debug adapters",
            ),
            count(
                if self.runs_code() {
                    self.context_servers.len()
                } else {
                    0
                },
                "context server",
                "context servers",
            ),
            count(self.themes.len(), "theme", "themes"),
            count(self.icon_themes.len(), "icon theme", "icon themes"),
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

    // Icon themes: the files the manifest names, and those in the folder
    // they are kept in.
    let mut icon_paths = files(&dir.join("icon_themes"), "json");
    for named in strings(&manifest["icon_themes"]) {
        if let Some(path) = inside(dir, &named).filter(|p| p.is_file())
            && !icon_paths.contains(&path)
        {
            icon_paths.push(path);
        }
    }
    let icon_themes = icon_paths
        .iter()
        .flat_map(|path| icons::read(dir, path))
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
            indents: query("indents.scm"),
            brackets: query("brackets.scm"),
            outline: query("outline.scm"),
            overrides: query("overrides.scm"),
        });
        // A comment with two ends is a pair of strings, or a table that
        // also says how its middle lines begin.
        let comment = &config["block_comment"];
        let (start, end) = match comment.as_array() {
            Some(ends) if ends.len() == 2 => (text(&ends[0]), text(&ends[1])),
            _ => (text(&comment["start"]), text(&comment["end"])),
        };
        let pattern = |one: &str, many: &str| {
            let mut patterns: Vec<String> = config[one]
                .as_str()
                .map(str::to_string)
                .into_iter()
                .collect();
            patterns.extend(
                config[many]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["pattern"].as_str().or(p.as_str()))
                    .map(str::to_string),
            );
            match patterns.len() {
                0 => None,
                1 => patterns.pop(),
                _ => Some(format!("(?:{})", patterns.join(")|(?:"))),
            }
        };
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
            block_comment: (!start.is_empty() && !end.is_empty()).then_some((start, end)),
            pairs: config["brackets"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|pair| Pair {
                    start: pair["start"].as_str().unwrap_or_default().to_string(),
                    end: pair["end"].as_str().unwrap_or_default().to_string(),
                    close: pair["close"].as_bool().unwrap_or(false),
                    newline: pair["newline"].as_bool().unwrap_or(false),
                    not_in: strings(&pair["not_in"]),
                })
                .filter(|pair| !pair.start.is_empty() && !pair.end.is_empty())
                .collect(),
            autoclose_before: config["autoclose_before"].as_str().map(str::to_string),
            word_characters: strings(&config["word_characters"]).concat(),
            completion_characters: strings(&config["completion_query_characters"]).concat(),
            debuggers: strings(&config["debuggers"]),
            increase_indent: pattern("increase_indent_pattern", "increase_indent_patterns"),
            decrease_indent: pattern("decrease_indent_pattern", "decrease_indent_patterns"),
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
        ("slash_commands", "Slash commands"),
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
        icon_themes,
        snippets,
        languages,
        servers,
        debug_adapters: manifest["debug_adapters"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, _)| name.clone())
            .collect(),
        context_servers: manifest["context_servers"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, _)| name.clone())
            .collect(),
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
            "{} of its themes (could not be read)",
            declared - themes.len()
        ));
    }

    // Icon themes, each under the name the extension shows it by. One
    // drawn with a font has no pictures to take.
    let mut icon_themes: Vec<IconTheme> = Vec::new();
    let mut drawn_with_a_font = 0;
    for entry in list("iconThemes") {
        let named = [&entry["label"], &entry["id"]]
            .into_iter()
            .map(|name| label(&text(name)))
            .find(|name| !name.is_empty());
        let read = named
            .zip(entry["path"].as_str().and_then(|p| inside(dir, p)))
            .map(|(name, path)| icons::read_vscode(dir, &path, &name))
            .unwrap_or_default();
        if read.is_empty() {
            drawn_with_a_font += 1;
        }
        icon_themes.extend(read);
    }
    if drawn_with_a_font > 0 {
        missing.push(match drawn_with_a_font {
            1 => "An icon theme (drawn with a font, or not readable)".to_string(),
            n => format!("{n} icon themes (drawn with a font, or not readable)"),
        });
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
            ..Default::default()
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
        icon_themes,
        snippets,
        languages,
        servers: Vec::new(),
        debug_adapters: Vec::new(),
        context_servers: Vec::new(),
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
