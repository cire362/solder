//! Icon themes of Zed extensions: which picture goes with which file.
//!
//! A theme names kinds of files (`rust`, `image`, `lock`) and gives each a
//! picture. Which file is of which kind it may say itself, by whole name or
//! by ending; for the rest there is the table at the end of this file, so
//! that a theme that only draws the kinds Zed knows works without saying
//! more.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use import::jsonc;
use serde_json::Value;

/// A folder's two pictures: closed and open.
type Folder = [Option<Arc<Path>>; 2];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IconTheme {
    pub name: String,
    pub dark: bool,
    folder: Folder,
    /// Folders that have pictures of their own, by name: `src`, `tests`.
    named_folders: HashMap<String, Folder>,
    /// The kind of a file, by its whole name and by its ending.
    stems: HashMap<String, String>,
    suffixes: HashMap<String, String>,
    /// The picture of each kind. Shared, since every row that shows a file
    /// of the kind hands the path to what draws it.
    icons: HashMap<String, Arc<Path>>,
}

impl IconTheme {
    /// The picture for a file called `name`. The whole name is looked up
    /// first, then each ending from the longest: `auth.module.ts` is
    /// `module.ts` before it is `ts`. A file of no known kind gets the
    /// theme's `default`.
    pub fn file(&self, name: &str) -> Option<&Arc<Path>> {
        let mut key = name;
        loop {
            if let Some(icon) = self.of_kind(key) {
                return Some(icon);
            }
            match key.split_once('.') {
                Some((_, rest)) => key = rest,
                None => break,
            }
        }
        self.icons.get("default")
    }

    fn of_kind(&self, key: &str) -> Option<&Arc<Path>> {
        let own = self.stems.get(key).or_else(|| self.suffixes.get(key));
        if let Some(icon) = own.and_then(|kind| self.icons.get(kind)) {
            return Some(icon);
        }
        // The kinds everyone knows, under the names a theme may have for
        // them: the first it has a picture for.
        kinds(key).iter().find_map(|kind| self.icons.get(*kind))
    }

    /// The picture for a folder called `name`, open or closed.
    pub fn folder(&self, name: &str, open: bool) -> Option<&Arc<Path>> {
        let state = usize::from(open);
        self.named_folders
            .get(name)
            .and_then(|folder| folder[state].as_ref())
            .or(self.folder[state].as_ref())
    }
}

/// A path the theme names, if it stays inside the extension.
fn inside(dir: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative.trim_start_matches("./"));
    let plain = relative
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    plain.then(|| dir.join(relative))
}

/// The themes in one file of an extension unpacked in `dir`. A file that
/// cannot be read holds none.
pub fn read(dir: &Path, file: &Path) -> Vec<IconTheme> {
    let Some(family) = std::fs::read_to_string(file)
        .ok()
        .and_then(|source| jsonc::parse(&source).ok())
    else {
        return Vec::new();
    };
    let picture =
        |value: &Value| -> Option<Arc<Path>> { Some(inside(dir, value.as_str()?)?.into()) };
    let folder = |value: &Value| [picture(&value["collapsed"]), picture(&value["expanded"])];
    let names = |value: &Value| -> HashMap<String, String> {
        value
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, kind)| Some((name.clone(), kind.as_str()?.to_string())))
            .collect()
    };
    family["themes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|theme| {
            let name = theme["name"].as_str()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(IconTheme {
                name,
                dark: theme["appearance"] != "light",
                folder: folder(&theme["directory_icons"]),
                named_folders: theme["named_directory_icons"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, icons)| (name.clone(), folder(icons)))
                    .collect(),
                stems: names(&theme["file_stems"]),
                suffixes: names(&theme["file_suffixes"]),
                icons: theme["file_icons"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(kind, icon)| Some((kind.clone(), picture(&icon["path"])?)))
                    .collect(),
            })
        })
        .collect()
}

/// The kinds a whole file name or an ending is known as, the most exact
/// first. These are the names Zed's own icon themes use, which is what
/// themes made for it draw.
fn kinds(key: &str) -> &'static [&'static str] {
    match key {
        "rs" => &["rust"],
        "ts" | "mts" | "cts" => &["typescript"],
        "tsx" | "jsx" => &["react"],
        "js" | "mjs" | "cjs" => &["javascript"],
        "json" | "jsonc" | "json5" => &["json", "storage"],
        "py" | "pyi" | "pyw" => &["python"],
        "go" => &["go"],
        "c" | "h" => &["c"],
        "cc" | "cpp" | "cxx" | "c++" | "hh" | "hpp" | "hxx" | "h++" | "ino" => &["cpp"],
        "cs" => &["csharp"],
        "fs" | "fsx" | "fsi" => &["fsharp"],
        "java" | "jar" => &["java"],
        "kt" | "kts" => &["kotlin"],
        "swift" => &["swift"],
        "rb" | "erb" | "gemspec" | "Gemfile" | "Rakefile" | "Podfile" => &["ruby"],
        "php" => &["php"],
        "lua" => &["lua"],
        "luau" => &["luau", "lua"],
        "zig" | "zon" => &["zig"],
        "ex" | "exs" | "eex" => &["elixir"],
        "heex" => &["heex", "elixir"],
        "erl" | "hrl" => &["erlang"],
        "hs" | "lhs" => &["haskell"],
        "ml" | "mli" => &["ocaml"],
        "scala" | "sc" | "sbt" => &["scala"],
        "dart" => &["dart"],
        "jl" => &["julia"],
        "nim" => &["nim"],
        "nix" => &["nix"],
        "r" | "R" | "rmd" => &["r"],
        "elm" => &["elm"],
        "gleam" => &["gleam"],
        "roc" => &["roc"],
        "tcl" => &["tcl"],
        "v" | "vsh" => &["v"],
        "odin" => &["odin"],
        "sol" => &["solidity"],
        "coffee" => &["coffeescript"],
        "vue" => &["vue"],
        "svelte" => &["svelte"],
        "astro" => &["astro"],
        "html" | "htm" | "xhtml" => &["html", "template"],
        "hbs" | "handlebars" | "mustache" | "tmpl" | "liquid" | "njk" | "ejs" => &["template"],
        "css" | "pcss" | "postcss" => &["css"],
        "sass" | "scss" | "less" | "styl" => &["sass", "css"],
        "md" | "mdx" | "markdown" => &["markdown", "document"],
        "txt" | "rtf" | "doc" | "docx" | "odt" | "pdf" | "LICENSE" | "README" => &["document"],
        "toml" => &["toml", "settings"],
        "yml" | "yaml" => &["yaml", "settings"],
        "ini" | "conf" | "cfg" | "env" | "editorconfig" | "properties" | "plist" => &["settings"],
        "xml" | "xsd" | "xsl" => &["code"],
        "lock" | "lockb" => &["lock"],
        "log" => &["log"],
        "diff" | "patch" => &["diff"],
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" | "nu" | "Makefile" | "Justfile"
        | "justfile" => &["terminal"],
        "sql" | "db" | "sqlite" | "sqlite3" | "csv" | "tsv" | "parquet" | "bak" => &["storage"],
        "git" | "gitignore" | "gitattributes" | "gitmodules" | "gitkeep" | "mailmap" => &["vcs"],
        "Dockerfile" | "dockerfile" | "dockerignore" | "Containerfile" => &["docker"],
        "tf" | "tfvars" => &["terraform"],
        "hcl" => &["hcl"],
        "gql" | "graphql" | "graphqls" => &["graphql"],
        "prisma" => &["prisma"],
        "kdl" => &["kdl"],
        "wgsl" => &["wgsl"],
        "metal" => &["metal"],
        "eslintrc" | "eslintrc.js" | "eslintrc.cjs" | "eslintrc.json" | "eslintignore"
        | "eslint.config.js" | "eslint.config.mjs" | "eslint.config.cjs" | "eslint.config.ts" => {
            &["eslint"]
        }
        "prettierrc"
        | "prettierignore"
        | "prettierrc.json"
        | "prettier.config.js"
        | "prettier.config.mjs"
        | "prettier.config.cjs" => &["prettier"],
        "stylelintrc" | "stylelintignore" | "stylelint.config.js" => &["stylelint"],
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "bmp" | "avif" | "heic"
        | "tiff" | "psd" => &["image"],
        "mp3" | "wav" | "ogg" | "flac" | "aac" | "m4a" | "opus" | "aiff" => &["audio"],
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => &["video"],
        "ttf" | "otf" | "woff" | "woff2" | "eot" => &["font"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{scratch, write};

    #[test]
    fn a_theme_gives_each_file_the_picture_of_its_kind() {
        let dir = scratch("icons");
        let file = dir.join("icon_themes/demo.json");
        write(
            &file,
            r#"{
  // Two themes of one family.
  "name": "Demo Icons",
  "themes": [
    {
      "name": "Demo Icons",
      "appearance": "dark",
      "directory_icons": { "collapsed": "./icons/folder.svg", "expanded": "./icons/folder-open.svg" },
      "named_directory_icons": { "src": { "collapsed": "./icons/src.svg" } },
      "file_stems": { "Cargo.toml": "cargo" },
      "file_suffixes": { "module.ts": "angular", "demo": "rust" },
      "file_icons": {
        "default": { "path": "./icons/file.svg" },
        "rust": { "path": "./icons/rust.svg" },
        "cargo": { "path": "./icons/cargo.svg" },
        "angular": { "path": "./icons/angular.svg" },
        "typescript": { "path": "./icons/ts.svg" },
        "storage": { "path": "./icons/storage.svg" },
        "vcs": { "path": "./icons/git.svg" },
        "outside": { "path": "../../secret.svg" }
      }
    },
    { "name": "Demo Icons Light", "appearance": "light", "file_icons": {} },
    { "appearance": "dark" }
  ]
}"#,
        );
        let themes = read(&dir, &file);
        assert_eq!(themes.len(), 2);
        assert_eq!(
            (themes[0].name.as_str(), themes[0].dark),
            ("Demo Icons", true)
        );
        assert_eq!(
            (themes[1].name.as_str(), themes[1].dark),
            ("Demo Icons Light", false)
        );
        let theme = &themes[0];
        let icon = |name: &str| {
            let path = theme.file(name).unwrap();
            path.strip_prefix(&dir)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        };
        // By the kinds everyone knows, by the theme's own endings and whole
        // names, and by the longest ending that is known.
        assert_eq!(icon("main.rs"), "icons/rust.svg");
        assert_eq!(icon("notes.demo"), "icons/rust.svg");
        assert_eq!(icon("Cargo.toml"), "icons/cargo.svg");
        assert_eq!(icon("auth.module.ts"), "icons/angular.svg");
        assert_eq!(icon("auth.ts"), "icons/ts.svg");
        assert_eq!(icon(".gitignore"), "icons/git.svg");
        // A kind under the older of its two names, and one the theme does
        // not draw at all.
        assert_eq!(icon("package.json"), "icons/storage.svg");
        assert_eq!(icon("other.toml"), "icons/file.svg");
        assert_eq!(icon("unknown"), "icons/file.svg");
        // A picture outside the extension is not one.
        assert!(!theme.icons.contains_key("outside"));

        let folder = |name: &str, open: bool| {
            let path = theme.folder(name, open)?;
            Some(
                path.strip_prefix(&dir)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string(),
            )
        };
        assert_eq!(folder("docs", false).as_deref(), Some("icons/folder.svg"));
        assert_eq!(
            folder("docs", true).as_deref(),
            Some("icons/folder-open.svg")
        );
        assert_eq!(folder("src", false).as_deref(), Some("icons/src.svg"));
        // It has no picture for `src` open: the common one.
        assert_eq!(
            folder("src", true).as_deref(),
            Some("icons/folder-open.svg")
        );
        // A theme that draws nothing gives nothing, and a file that is not
        // a theme holds none.
        assert_eq!(themes[1].file("main.rs"), None);
        assert_eq!(themes[1].folder("src", true), None);
        assert!(read(&dir, &dir.join("missing.json")).is_empty());
    }
}
