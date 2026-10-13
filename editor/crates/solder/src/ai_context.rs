use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use ignore::{
    WalkBuilder,
    gitignore::{Gitignore, GitignoreBuilder},
};

use crate::{chat_panel::FileContext, project::is_excluded_dir};

const IGNORE_LIMIT: u64 = 64 * 1024;
const SCAN_LIMIT: usize = 20_000;
const FILE_LIMIT: usize = 1_000;
const MAP_LIMIT: usize = 12_000;
const SOURCE_LIMIT: u64 = 256 * 1024;
const SOURCE_BUDGET: u64 = 2 * 1024 * 1024;

pub struct Input {
    pub root: PathBuf,
    pub file: Option<FileContext>,
    pub project: bool,
    pub question: String,
    pub history: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct Prepared {
    pub file: Option<FileContext>,
    pub map: Option<ProjectMap>,
}

#[derive(Debug)]
pub struct ProjectMap {
    pub text: String,
    pub paths: Vec<PathBuf>,
    pub limited: bool,
}

pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct Policy {
    root: PathBuf,
    original_root: PathBuf,
    ignore: Gitignore,
    rules: Option<String>,
}

impl Policy {
    fn load(root: &Path) -> Result<Self, String> {
        if !ai::context::allows_file(root) {
            return Err(crate::lang::t("This project folder is excluded from AI context.").into());
        }
        let canonical = crate::remote::canonicalize(root)
            .map_err(|_| crate::lang::t("Cannot read the project folder."))?;
        if !ai::context::allows_file(&canonical) {
            return Err(crate::lang::t("This project folder is excluded from AI context.").into());
        }
        let path = canonical.join(".solderignore");
        let mut builder = GitignoreBuilder::new(&canonical);
        let mut rules = None;
        if crate::remote::is_remote(&canonical) {
            let checked = checked_path(&canonical, &path)?;
            match crate::remote::metadata(&checked) {
                Ok(metadata) => {
                    if !metadata.is_file() || metadata.is_symlink() || metadata.len() > IGNORE_LIMIT
                    {
                        return Err(crate::lang::t("Cannot read .solderignore safely.").into());
                    }
                    let bytes = crate::remote::read(&checked)
                        .map_err(|_| crate::lang::t("Cannot read .solderignore."))?;
                    checked_path(&canonical, &checked)?;
                    rules = Some(parse_rules(&path, bytes, &mut builder)?);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(crate::lang::t("Cannot read .solderignore.").into()),
            }
        } else {
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if !metadata.is_file() || metadata.len() > IGNORE_LIMIT {
                        return Err(crate::lang::t("Cannot read .solderignore safely.").into());
                    }
                    let mut bytes = Vec::new();
                    File::open(&path)
                        .and_then(|file| file.take(IGNORE_LIMIT + 1).read_to_end(&mut bytes))
                        .map_err(|_| crate::lang::t("Cannot read .solderignore."))?;
                    rules = Some(parse_rules(&path, bytes, &mut builder)?);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(crate::lang::t("Cannot read .solderignore.").into()),
            }
        }
        Ok(Self {
            root: canonical,
            original_root: root.to_path_buf(),
            ignore: builder
                .build()
                .map_err(|_| crate::lang::t("Invalid pattern in .solderignore."))?,
            rules,
        })
    }

    fn relative<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        let relative = path
            .strip_prefix(&self.root)
            .or_else(|_| path.strip_prefix(&self.original_root))
            .ok()?;
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return None;
        }
        Some(relative)
    }

    fn allows(&self, path: &Path, directory: bool) -> bool {
        let Some(relative) = self.relative(path) else {
            return false;
        };
        ai::context::allows_file(relative)
            && !relative
                .components()
                .any(|component| component.as_os_str().to_str().is_some_and(is_excluded_dir))
            && relative
                .file_name()
                .is_none_or(|name| name != ".solderignore" && name != ".DS_Store")
            && !self
                .ignore
                .matched_path_or_any_parents(relative, directory)
                .is_ignore()
    }

    fn indexed(&self, path: &Path, files: &HashSet<PathBuf>) -> bool {
        self.relative(path)
            .is_some_and(|relative| files.contains(&self.root.join(relative)))
    }
}

fn parse_rules(
    path: &Path,
    bytes: Vec<u8>,
    builder: &mut GitignoreBuilder,
) -> Result<String, String> {
    if bytes.len() as u64 > IGNORE_LIMIT {
        return Err(crate::lang::t(".solderignore is too large.").into());
    }
    let source =
        String::from_utf8(bytes).map_err(|_| crate::lang::t(".solderignore must be UTF-8."))?;
    for line in source.lines() {
        builder
            .add_line(Some(path.to_path_buf()), line)
            .map_err(|_| crate::lang::t("Invalid pattern in .solderignore."))?;
    }
    Ok(source)
}

/// Resolve an existing path or its nearest existing parent before letting
/// a file tool reach it. Target symlinks are rejected by the transport.
pub(crate) fn checked_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    checked_path_with(root, path, crate::remote::canonicalize, |path| {
        if crate::remote::is_remote(path) {
            crate::remote::metadata(path).map(|metadata| metadata.is_symlink())
        } else {
            fs::symlink_metadata(path).map(|metadata| metadata.file_type().is_symlink())
        }
    })
}

fn checked_path_with(
    root: &Path,
    path: &Path,
    canonicalize: impl Fn(&Path) -> std::io::Result<PathBuf>,
    is_symlink: impl Fn(&Path) -> std::io::Result<bool>,
) -> Result<PathBuf, String> {
    let denied = || crate::lang::t("Cannot check the project's context rules.").to_string();
    let canonical_root = canonicalize(root).map_err(|_| denied())?;
    let relative = path
        .strip_prefix(root)
        .or_else(|_| path.strip_prefix(&canonical_root))
        .map_err(|_| denied())?;
    if relative
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(denied());
    }
    let mut parent = path;
    let mut missing = Vec::new();
    let mut checked = loop {
        match canonicalize(parent) {
            Ok(checked) => break checked,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && parent != root => {
                match is_symlink(parent) {
                    Ok(_) => return Err(denied()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(denied()),
                }
                missing.push(parent.file_name().ok_or_else(denied)?.to_os_string());
                parent = parent.parent().ok_or_else(denied)?;
            }
            Err(_) => return Err(denied()),
        }
    };
    if !checked.starts_with(&canonical_root) {
        return Err(denied());
    }
    for part in missing.into_iter().rev() {
        checked.push(part);
    }
    Ok(checked)
}

/// Whether one file may go to a model: the `.env` rule and the project's
/// `.solderignore`, without walking the project. Blocks on a small read.
pub fn allows_path(root: &Path, path: &Path) -> Result<bool, String> {
    if !ai::context::allows_file(path) {
        return Ok(false);
    }
    let policy = Policy::load(root)?;
    Ok(checked_path(root, path).is_ok_and(|path| policy.allows(&path, false)))
}

pub fn prepare(input: Input, cancelled: &AtomicBool) -> Result<Prepared, String> {
    let stopped = || cancelled.load(Ordering::Relaxed);
    if stopped() {
        return Err(crate::lang::t("Context preparation stopped.").into());
    }
    if !input.project && input.file.is_none() && input.history.is_empty() {
        return Ok(Prepared {
            file: None,
            map: None,
        });
    }
    let policy = Arc::new(Policy::load(&input.root)?);
    if crate::remote::is_remote(&policy.root) {
        return prepare_remote(input, &policy, cancelled);
    }
    let filter_policy = policy.clone();
    let mut walker = WalkBuilder::new(&policy.root);
    walker
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            entry.depth() == 0
                || filter_policy.allows(
                    entry.path(),
                    entry.file_type().is_some_and(|kind| kind.is_dir()),
                )
        });
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut paths = Vec::new();
    let mut limited = false;
    for (visited, entry) in walker.build().enumerate() {
        if stopped() {
            return Err(crate::lang::t("Context preparation stopped.").into());
        }
        if visited >= SCAN_LIMIT || Instant::now() >= deadline {
            limited = true;
            break;
        }
        let entry =
            entry.map_err(|_| crate::lang::t("Cannot check the project's context rules."))?;
        if entry.error().is_some() {
            return Err(crate::lang::t("Cannot check the project's context rules.").into());
        }
        if entry.file_type().is_some_and(|kind| kind.is_file()) && entry.path().to_str().is_some() {
            paths.push(entry.into_path());
        }
    }
    let files: HashSet<_> = paths.iter().cloned().collect();
    if input
        .history
        .iter()
        .any(|path| !policy.indexed(path, &files))
    {
        return Err(crate::lang::t("Context rules changed. Start a new chat.").into());
    }
    let file = input
        .file
        .filter(|file| policy.indexed(&file.source, &files));
    let map = if input.project {
        Some(build_map(
            &policy,
            paths,
            &input.question,
            file.as_ref(),
            limited,
            cancelled,
        )?)
    } else {
        None
    };
    if Policy::load(&input.root)?.rules != policy.rules {
        return Err(crate::lang::t("Context rules changed. Try again.").into());
    }
    Ok(Prepared { file, map })
}

fn prepare_remote(
    input: Input,
    policy: &Policy,
    cancelled: &AtomicBool,
) -> Result<Prepared, String> {
    let (entries, limited) = remote_entries(
        &input.root,
        &policy.root,
        usize::MAX,
        SCAN_LIMIT,
        true,
        cancelled,
    )?;
    let paths: Vec<_> = entries
        .into_iter()
        .filter(|entry| !entry.directory)
        .map(|entry| entry.path)
        .collect();
    let files: HashSet<_> = paths.iter().cloned().collect();
    if input
        .history
        .iter()
        .any(|path| !policy.indexed(path, &files))
    {
        return Err(crate::lang::t("Context rules changed. Start a new chat.").into());
    }
    let file = input
        .file
        .filter(|file| policy.indexed(&file.source, &files));
    let map = if input.project {
        Some(build_map(
            policy,
            paths,
            &input.question,
            file.as_ref(),
            limited,
            cancelled,
        )?)
    } else {
        None
    };
    if Policy::load(&input.root)?.rules != policy.rules {
        return Err(crate::lang::t("Context rules changed. Try again.").into());
    }
    Ok(Prepared { file, map })
}

pub(crate) struct RemoteEntry {
    pub path: PathBuf,
    pub directory: bool,
}

/// Mirror names are an inventory only. File types, canonical paths and
/// ignore-file bytes are checked on the target before they are used.
pub(crate) fn remote_entries(
    root: &Path,
    dir: &Path,
    depth: usize,
    limit: usize,
    respect_ignore: bool,
    cancelled: &AtomicBool,
) -> Result<(Vec<RemoteEntry>, bool), String> {
    let policy = Policy::load(root)?;
    let dir = checked_path(root, dir)?;
    let failed = || crate::lang::t("Cannot check the project's context rules.").to_string();
    if cancelled.load(Ordering::Relaxed) {
        return Err(crate::lang::t("Context preparation stopped.").into());
    }
    let mut inherited = Vec::new();
    if respect_ignore {
        let mut ancestor = policy.root.clone();
        for part in dir
            .strip_prefix(&policy.root)
            .map_err(|_| failed())?
            .components()
        {
            inherited.extend(remote_ignore(&policy.root, &ancestor)?);
            ancestor.push(part);
        }
    }
    let metadata = crate::remote::metadata(&dir).map_err(|_| failed())?;
    if metadata.is_file() {
        let allowed = policy.allows(&dir, false) && !ignored_by(&inherited, &dir, false);
        return Ok((
            if allowed {
                vec![RemoteEntry {
                    path: dir,
                    directory: false,
                }]
            } else {
                Vec::new()
            },
            false,
        ));
    }
    if !metadata.is_dir() || metadata.is_symlink() {
        return Err(failed());
    }
    crate::remote::sync_tree(&dir).map_err(|_| failed())?;
    let mut stack = vec![(dir, depth, inherited)];
    let mut entries = Vec::new();
    let mut visited = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    while let Some((dir, depth, mut rules)) = stack.pop() {
        if respect_ignore {
            rules.extend(remote_ignore(&policy.root, &dir)?);
        }
        // read_dir supplies names, never bytes or trusted file types.
        let mut names: Vec<_> = fs::read_dir(&dir)
            .map_err(|_| failed())?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<_>>()
            .map_err(|_| failed())?;
        names.sort();
        let mut children = Vec::new();
        for name in names {
            if cancelled.load(Ordering::Relaxed) {
                return Err(crate::lang::t("Context preparation stopped.").into());
            }
            if visited >= limit || Instant::now() >= deadline {
                return Ok((entries, true));
            }
            visited += 1;
            let path = dir.join(name);
            // Remote agent checkouts live inside this repository. Walking
            // them here would attach duplicate trees and other tasks.
            if path
                .strip_prefix(&policy.root)
                .is_ok_and(|relative| relative.starts_with(".solder/worktrees"))
            {
                continue;
            }
            if !policy.allows(&path, false) && !policy.allows(&path, true) {
                continue;
            }
            // A symlink may name private data even if its own name is
            // allowed. It contributes neither a file nor a directory.
            let Ok(path) = checked_path(&policy.root, &path) else {
                continue;
            };
            let metadata = match crate::remote::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(failed()),
            };
            if metadata.is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
                continue;
            }
            let directory = metadata.is_dir();
            if !policy.allows(&path, directory) || ignored_by(&rules, &path, directory) {
                continue;
            }
            if directory && depth > 1 {
                children.push((path.clone(), depth - 1, rules.clone()));
            }
            if path.to_str().is_some() {
                entries.push(RemoteEntry { path, directory });
            }
        }
        stack.extend(children.into_iter().rev());
    }
    Ok((entries, false))
}

fn ignored_by(rules: &[Arc<Gitignore>], path: &Path, directory: bool) -> bool {
    for rule in rules.iter().rev() {
        let matched = rule.matched_path_or_any_parents(path, directory);
        if !matched.is_none() {
            return matched.is_ignore();
        }
    }
    false
}

fn remote_ignore(root: &Path, dir: &Path) -> Result<Vec<Arc<Gitignore>>, String> {
    let failed = || crate::lang::t("Cannot check the project's context rules.").to_string();
    let mut rules = Vec::new();
    for name in [".gitignore", ".ignore"] {
        let path = checked_path(root, &dir.join(name))?;
        let metadata = match crate::remote::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(failed()),
        };
        if !metadata.is_file() || metadata.is_symlink() || metadata.len() > IGNORE_LIMIT {
            return Err(failed());
        }
        let bytes = crate::remote::read(&path).map_err(|_| failed())?;
        if bytes.len() as u64 > IGNORE_LIMIT {
            return Err(failed());
        }
        checked_path(root, &path)?;
        let source = String::from_utf8(bytes).map_err(|_| failed())?;
        let mut builder = GitignoreBuilder::new(dir);
        for line in source.lines() {
            builder
                .add_line(Some(path.clone()), line)
                .map_err(|_| failed())?;
        }
        rules.push(Arc::new(builder.build().map_err(|_| failed())?));
    }
    Ok(rules)
}

fn build_map(
    policy: &Policy,
    mut paths: Vec<PathBuf>,
    question: &str,
    file: Option<&FileContext>,
    mut limited: bool,
    cancelled: &AtomicBool,
) -> Result<ProjectMap, String> {
    let words: Vec<_> = question
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| word.len() > 2)
        .take(32)
        .map(str::to_lowercase)
        .collect();
    let rank = |path: &Path| {
        let relative = policy.relative(path).unwrap_or(path);
        let name = relative.to_string_lossy().to_lowercase();
        let matches = words
            .iter()
            .filter(|word| name.contains(word.as_str()))
            .count();
        let nearby = file.is_some_and(|file| {
            policy.relative(&file.source).and_then(Path::parent) == relative.parent()
        });
        (std::cmp::Reverse(matches), std::cmp::Reverse(nearby))
    };
    paths.sort_by_cached_key(|path| (rank(path), path.clone()));
    limited |= paths.len() > FILE_LIMIT;
    let mut text = String::new();
    let mut included = Vec::new();
    let mut budget = SOURCE_BUDGET;
    for path in paths.into_iter().take(FILE_LIMIT) {
        if cancelled.load(Ordering::Relaxed) {
            return Err(crate::lang::t("Context preparation stopped.").into());
        }
        let relative = policy.relative(&path).unwrap();
        let mut entry = serde_json::to_string(&relative.to_string_lossy()).unwrap();
        let extension = path.extension().and_then(|extension| extension.to_str());
        // Declarations of the languages Solder knows, and of an extension's
        // language that has a query for them.
        let outlined = matches!(
            extension,
            Some(
                "rs" | "ts"
                    | "mts"
                    | "cts"
                    | "tsx"
                    | "js"
                    | "mjs"
                    | "cjs"
                    | "jsx"
                    | "go"
                    | "py"
                    | "pyi"
                    | "c"
                    | "h"
                    | "cc"
                    | "cpp"
                    | "cxx"
                    | "hh"
                    | "hpp"
                    | "hxx"
                    | "sh"
                    | "bash"
                    | "zsh"
                    | "md"
                    | "markdown"
            )
        ) || syntax::language_for_path(&path).is_some_and(|l| l.has_outline());
        if outlined && let Some(source) = read_source(&path, &mut budget) {
            for symbol in syntax::outline(&path, &source, 16, || cancelled.load(Ordering::Relaxed))
            {
                entry.push_str(&format!(
                    "\n  {} {}:{}",
                    symbol.kind,
                    serde_json::to_string(&symbol.name).unwrap(),
                    symbol.line
                ));
            }
        }
        entry.push('\n');
        if text.len() + entry.len() > MAP_LIMIT {
            limited = true;
            break;
        }
        text.push_str(&entry);
        included.push(path);
    }
    Ok(ProjectMap {
        text,
        paths: included,
        limited,
    })
}

fn read_source(path: &Path, budget: &mut u64) -> Option<String> {
    if crate::remote::is_remote(path) {
        if crate::remote::canonicalize(path).ok()?.as_path() != path {
            return None;
        }
        let metadata = crate::remote::metadata(path).ok()?;
        let size = metadata.len();
        if !metadata.is_file() || metadata.is_symlink() || size > SOURCE_LIMIT || size > *budget {
            return None;
        }
        *budget -= size;
        let bytes = crate::remote::read(path).ok()?;
        let after = crate::remote::metadata(path).ok()?;
        if bytes.len() as u64 != size
            || bytes.contains(&0)
            || after.len() != size
            || after.modified().ok()? != metadata.modified().ok()?
            || crate::remote::canonicalize(path).ok()?.as_path() != path
        {
            return None;
        }
        return String::from_utf8(bytes).ok();
    }
    if path.canonicalize().ok()?.as_path() != path {
        return None;
    }
    let file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    if size > SOURCE_LIMIT || size > *budget {
        return None;
    }
    *budget -= size;
    let mut bytes = Vec::new();
    file.take(size + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 != size
        || bytes.contains(&0)
        || path.canonicalize().ok()?.as_path() != path
    {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_canonicalization_fails_closed_and_checks_missing_parents() {
        let root = Path::new("/virtual/cache/repo");
        let missing = root.join("new/file.rs");
        let absent = || std::io::Error::new(std::io::ErrorKind::NotFound, "absent on target");
        let checked = checked_path_with(
            root,
            &missing,
            |path| {
                if path == root {
                    Ok(root.to_path_buf())
                } else {
                    Err(absent())
                }
            },
            |_| Err(absent()),
        )
        .unwrap();
        assert_eq!(checked, missing);
        assert!(
            checked_path_with(
                root,
                &missing,
                |path| {
                    if path == root {
                        Ok(root.to_path_buf())
                    } else if path == root.join("new") {
                        Ok("/outside".into())
                    } else {
                        Err(absent())
                    }
                },
                |_| Err(absent())
            )
            .is_err()
        );
        assert!(
            checked_path_with(
                root,
                &missing,
                |path| {
                    if path == root {
                        Ok(root.to_path_buf())
                    } else {
                        Err(absent())
                    }
                },
                |path| if path == root.join("new") {
                    Ok(true)
                } else {
                    Err(absent())
                }
            )
            .is_err()
        );
        assert!(
            checked_path_with(
                root,
                &missing,
                |_| Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "closed mount"
                )),
                |_| panic!("must not fall back to cache")
            )
            .is_err()
        );
        assert!(
            checked_path_with(
                root,
                &root.join("../outside"),
                |path| Ok(path.to_path_buf()),
                |_| panic!("parent traversal")
            )
            .is_err()
        );
    }

    fn fixture(name: &str) -> PathBuf {
        let root = db::testing::dir(name);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/app.rs"),
            "pub fn app() { let value = \"BODY_SECRET\"; }",
        )
        .unwrap();
        fs::write(root.join("src/hidden.rs"), "fn PRIVATE() {}").unwrap();
        fs::write(root.join(".env"), "ENV_SECRET=1").unwrap();
        fs::write(root.join(".ENV.local"), "ENV_SECRET=2").unwrap();
        fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        fs::create_dir_all(root.join("ignored")).unwrap();
        fs::write(root.join("ignored/file.rs"), "fn GIT_SECRET() {}").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/file.rs"), "fn BUILD_SECRET() {}").unwrap();
        root.canonicalize().unwrap()
    }

    fn input(root: &Path) -> Input {
        Input {
            root: root.into(),
            file: None,
            project: true,
            question: "app".into(),
            history: Vec::new(),
        }
    }

    fn context(root: &Path, relative: &str) -> FileContext {
        FileContext {
            source: root.join(relative),
            path: relative.into(),
            text: "SELECTED_SECRET".into(),
            part: Some("line 1".into()),
        }
    }

    #[test]
    fn map_has_declarations_not_bodies_and_respects_all_exclusions() {
        let root = fixture("ai-map-privacy");
        fs::write(
            root.join(".solderignore"),
            "src/*\n!src/app.rs\n!.env\n!.ENV.local\n!target/\n!ignored/\n",
        )
        .unwrap();
        let map = prepare(input(&root), &AtomicBool::new(false))
            .unwrap()
            .map
            .unwrap();
        assert!(map.text.contains("src/app.rs"));
        assert!(map.text.contains("fn \"app\":1"));
        for private in [
            ".env",
            ".ENV",
            "hidden.rs",
            "PRIVATE",
            "BODY_SECRET",
            "ENV_SECRET",
            "GIT_SECRET",
            "BUILD_SECRET",
            "target/",
            "ignored/",
        ] {
            assert!(!map.text.contains(private), "{private}: {}", map.text);
        }
        assert!(!map.limited);
    }

    #[test]
    fn file_and_history_use_fresh_rules() {
        let root = fixture("ai-map-history");
        let mut request = input(&root);
        request.project = false;
        request.file = Some(context(&root, "src/app.rs"));
        assert!(
            prepare(request, &AtomicBool::new(false))
                .unwrap()
                .file
                .is_some()
        );
        fs::write(root.join(".solderignore"), "src/app.rs\n").unwrap();
        let mut request = input(&root);
        request.file = Some(context(&root, "src/app.rs"));
        assert!(
            prepare(request, &AtomicBool::new(false))
                .unwrap()
                .file
                .is_none()
        );
        let mut request = input(&root);
        request.history.push(root.join("src/app.rs"));
        assert!(
            prepare(request, &AtomicBool::new(false))
                .unwrap_err()
                .contains("Start a new chat")
        );
        fs::write(root.join(".solderignore"), "").unwrap();
        let mut request = input(&root);
        request.history.push(root.join("src/app.rs"));
        fs::write(root.join("new.rs"), "fn added() {}").unwrap();
        assert!(prepare(request, &AtomicBool::new(false)).is_ok());
    }

    #[test]
    fn bad_rules_fail_closed_but_plain_questions_do_not_scan() {
        let root = fixture("ai-map-bad-rules");
        for rules in ["[z-a]".into(), "x".repeat(IGNORE_LIMIT as usize + 1)] {
            fs::write(root.join(".solderignore"), rules).unwrap();
            assert!(prepare(input(&root), &AtomicBool::new(false)).is_err());
            let mut request = input(&root);
            request.project = false;
            assert!(prepare(request, &AtomicBool::new(false)).is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_outside_paths_and_env_selections_are_never_attached() {
        let root = fixture("ai-map-links");
        std::os::unix::fs::symlink(root.join(".env"), root.join("src/link.rs")).unwrap();
        std::os::unix::fs::symlink(root.join("ignored"), root.join("alias")).unwrap();
        let outside = fixture("ai-map-outside");
        std::os::unix::fs::symlink(&outside, root.join("outside")).unwrap();
        for path in [
            root.join(".env"),
            root.join(".ENV.local"),
            root.join("src/link.rs"),
            root.join("alias/file.rs"),
            outside.join("src/app.rs"),
            root.join("src/../.env"),
        ] {
            let mut request = input(&root);
            request.file = Some(FileContext {
                source: path,
                ..context(&root, "src/app.rs")
            });
            let prepared = prepare(request, &AtomicBool::new(false)).unwrap();
            assert!(prepared.file.is_none());
            assert!(!prepared.map.unwrap().text.contains("link.rs"));
        }
        std::os::unix::fs::symlink(outside.join("src/app.rs"), root.join(".solderignore")).unwrap();
        assert!(prepare(input(&root), &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn bounded_deterministic_map_and_cancellation() {
        let root = fixture("ai-map-limits");
        for number in 0..1_100 {
            fs::write(
                root.join(format!("src/file-{number:04}.rs")),
                "fn worker() {}",
            )
            .unwrap();
        }
        let first = prepare(input(&root), &AtomicBool::new(false))
            .unwrap()
            .map
            .unwrap();
        let second = prepare(input(&root), &AtomicBool::new(false))
            .unwrap()
            .map
            .unwrap();
        assert_eq!(first.text, second.text);
        assert!(first.text.len() <= MAP_LIMIT);
        assert!(first.paths.len() <= FILE_LIMIT);
        assert!(first.limited);
        assert!(first.text.starts_with("\"src/app.rs\""));
        assert!(prepare(input(&root), &AtomicBool::new(true)).is_err());
        let cancellation = Cancellation::new();
        let flag = cancellation.flag();
        drop(cancellation);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn binary_large_sources_and_path_newlines_do_not_expand_context() {
        let root = fixture("ai-map-source-limits");
        fs::write(root.join("src/binary.rs"), b"fn BINARY_SECRET() {}\0").unwrap();
        fs::write(
            root.join("src/large.rs"),
            format!(
                "fn LARGE_SECRET() {{}}{}",
                " ".repeat(SOURCE_LIMIT as usize)
            ),
        )
        .unwrap();
        fs::write(root.join("src/line\nbreak.rs"), "fn safe() {}").unwrap();
        let map = prepare(input(&root), &AtomicBool::new(false))
            .unwrap()
            .map
            .unwrap();
        assert!(!map.text.contains("BINARY_SECRET"));
        assert!(!map.text.contains("LARGE_SECRET"));
        assert!(map.text.contains("line\\nbreak.rs"));
    }

    #[test]
    fn checks_one_file_without_walking_the_project() {
        let root = fixture("ai-allows-path");
        fs::write(root.join(".solderignore"), "secret/\n*.pem\n").unwrap();
        fs::create_dir_all(root.join("secret")).unwrap();
        for (path, allowed) in [
            ("src/app.rs", true),
            ("secret/key.rs", false),
            ("certs/server.pem", false),
            (".env", false),
            ("config/.env.local", false),
            ("node_modules/x/index.js", false),
        ] {
            assert_eq!(
                allows_path(&root, &root.join(path)).unwrap(),
                allowed,
                "{path}"
            );
        }
        // Outside the project nothing is known to be allowed.
        assert!(!allows_path(&root, Path::new("/elsewhere/app.rs")).unwrap());
    }
}
