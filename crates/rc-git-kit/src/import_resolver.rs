//! Port of `ImportResolver.swift`: turns a module specifier written in a JavaScript or
//! TypeScript file into the repository path of the file it names, the way a bundler would.
//!
//! A relative specifier resolves against the importing file; anything else goes through the
//! `paths` and `baseUrl` of the nearest `tsconfig.json` or `jsconfig.json`. A specifier that
//! names a package from a registry resolves to `None`, since nothing in the tree answers for
//! it.
//!
//! Every answer comes from an [`ImportSource`], so resolution reads only the directories on
//! the way to a candidate and, for a specifier that is not relative, the config files above
//! the importing file. Nothing is walked and nothing is kept between calls. A source fails
//! when a read fails, as distinct from finding nothing, and [`ImportResolver::resolve`] passes
//! that error on, so an unreadable directory is never mistaken for a module that is not in the
//! tree.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use jsonc_parser::{JsonObject, JsonValue, ParseOptions, parse_to_value};
use rc_git_core::{BlobContent, EntryKind, Oid, Repository, TreeEntry};

use crate::repo_path::flatten;

/// Extensions tried on a specifier that leaves its own out, in TypeScript's order.
pub const EXTENSIONS: [&str; 10] = [
    ".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs", ".json",
];
/// The sources a compiled extension may stand for: TypeScript's ESM convention writes
/// `./module.js` for a file that is `module.ts` in the tree.
const SOURCE_EXTENSIONS: [(&str, &[&str]); 4] = [
    (".js", &[".ts", ".tsx"]),
    (".jsx", &[".tsx"]),
    (".mjs", &[".mts"]),
    (".cjs", &[".cts"]),
];
/// The config files looked for in each directory, in order.
pub const CONFIG_NAMES: [&str; 2] = ["tsconfig.json", "jsconfig.json"];
/// How many `extends` a config may chain through before the rest is ignored.
pub const EXTENDS_DEPTH: usize = 4;
/// How many config files one `resolve` may read, across ancestors, references and `extends`,
/// so a committed config that fans out cannot multiply the work of one click.
pub const CONFIG_READ_LIMIT: usize = 16;
/// The largest config file the live source reads, in bytes.
pub const CONFIG_SIZE_LIMIT: u64 = 256 * 1024;

/// Where an [`ImportResolver`] reads the tree. Paths are repository-relative with `/`
/// separators, `""` for the root.
pub trait ImportSource {
    /// What a failed read reports.
    type Error;

    /// The children of the directory at `directory`; `None` when it is not a directory.
    ///
    /// # Errors
    /// When the directory exists and could not be read.
    fn list(&self, directory: &str) -> Result<Option<Vec<TreeEntry>>, Self::Error>;

    /// The bytes of the config file at `path`; `None` when it cannot be read as text.
    ///
    /// # Errors
    /// When the file exists and could not be read.
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, Self::Error>;
}

/// Resolves module specifiers over one [`ImportSource`].
#[derive(Debug, Clone)]
pub struct ImportResolver<S> {
    source: S,
}

/// What a relative specifier names.
enum RelativeKind {
    File,
    Directory,
}

/// `compilerOptions.paths`: each pattern and its targets, in no particular order.
type Patterns = Vec<(String, Vec<String>)>;

/// What a config says about module names, with every path already made repository-relative.
#[derive(Default)]
struct ModulePaths {
    /// `compilerOptions.baseUrl`, the directory bare specifiers and `paths` start from.
    base_url: Option<Vec<String>>,
    /// `compilerOptions.paths` and the directory of the config that wrote them, which is what
    /// the targets start from when no `baseUrl` is set.
    paths: Option<(Patterns, Vec<String>)>,
    /// Where a solution-style config points: the configs it hands its files to.
    references: Vec<Vec<String>>,
}

impl ModulePaths {
    /// Takes `base_url` and `paths` from an extended config wherever this one has none.
    fn fill_unset(&mut self, inherited: Self) {
        if self.base_url.is_none() {
            self.base_url = inherited.base_url;
        }
        if self.paths.is_none() {
            self.paths = inherited.paths;
        }
    }
}

/// The config reads one resolution has left, shared by everything it reads.
struct ReadBudget {
    remaining: Cell<usize>,
}

impl ReadBudget {
    fn new() -> Self {
        Self {
            remaining: Cell::new(CONFIG_READ_LIMIT),
        }
    }

    fn is_spent(&self) -> bool {
        self.remaining.get() == 0
    }

    /// Takes one read from the budget. `false` once it is spent.
    fn spend(&self) -> bool {
        let remaining = self.remaining.get();
        if remaining == 0 {
            return false;
        }
        self.remaining.set(remaining - 1);
        true
    }
}

impl<S: ImportSource> ImportResolver<S> {
    /// A resolver reading through `source`.
    #[must_use]
    pub fn new(source: S) -> Self {
        Self { source }
    }

    /// Resolves `specifier` as written in the file at `source_path`. `None` when it names
    /// nothing in the tree. A `?query` suffix is not part of the path.
    ///
    /// # Errors
    /// Whatever the source returned for a directory or config on the way.
    pub fn resolve(&self, specifier: &str, source_path: &str) -> Result<Option<String>, S::Error> {
        let specifier = specifier.split('?').next().unwrap_or_default();
        if specifier.is_empty() || specifier.starts_with('/') {
            return Ok(None);
        }
        let mut directory = components(source_path);
        directory.pop();
        let resolution = Resolution {
            source: &self.source,
            budget: ReadBudget::new(),
        };
        match relative_kind(specifier) {
            Some(RelativeKind::File) => match flatten(&directory, specifier) {
                Some(path) => resolution.probe(&path),
                None => Ok(None),
            },
            Some(RelativeKind::Directory) => match flatten(&directory, specifier) {
                Some(path) => resolution.index_file(&path),
                None => Ok(None),
            },
            None => resolution.resolve_through_config(specifier, directory),
        }
    }
}

/// One resolution in progress: the source it reads and the config reads it has left.
struct Resolution<'s, S> {
    source: &'s S,
    budget: ReadBudget,
}

impl<S: ImportSource> Resolution<'_, S> {
    fn list(&self, directory: &[String]) -> Result<Option<Vec<TreeEntry>>, S::Error> {
        self.source.list(&directory.join("/"))
    }

    /// The file a path names once extensions are settled: the path itself, the path with a
    /// source extension, the source of a compiled extension, or the index file of the
    /// directory it names. An empty path is the repository root.
    fn probe(&self, components: &[String]) -> Result<Option<String>, S::Error> {
        let Some((name, parent)) = components.split_last() else {
            return self.index_file(&[]);
        };
        let Some(siblings) = self.list(parent)? else {
            return Ok(None);
        };
        for candidate in file_candidates(name) {
            if is_file(&candidate, &siblings) {
                return Ok(Some(joined(parent, &candidate)));
            }
        }
        let is_directory = siblings
            .iter()
            .any(|entry| entry.name == name.as_bytes() && entry.kind == EntryKind::Tree);
        if !is_directory {
            return Ok(None);
        }
        self.index_file(components)
    }

    fn index_file(&self, directory: &[String]) -> Result<Option<String>, S::Error> {
        let Some(entries) = self.list(directory)? else {
            return Ok(None);
        };
        for extension in EXTENSIONS {
            let name = format!("index{extension}");
            if is_file(&name, &entries) {
                return Ok(Some(joined(directory, &name)));
            }
        }
        Ok(None)
    }

    /// Resolves through the configs above `directory`, nearest first, and stops at the first
    /// that places the specifier. TypeScript asks only the nearest; asking the rest costs a
    /// read per config on a specifier nothing places, and finds the alias a root config
    /// declares for an editor. A config that only points at others, as a Vite project's does,
    /// has each one it references tried in order.
    fn resolve_through_config(
        &self,
        specifier: &str,
        mut current: Vec<String>,
    ) -> Result<Option<String>, S::Error> {
        loop {
            if let Some(entries) = self.list(&current)? {
                for name in CONFIG_NAMES {
                    if !is_file(name, &entries) {
                        continue;
                    }
                    let mut path = current.clone();
                    path.push(name.to_owned());
                    if let Some(found) = self.resolve_with_config_at(specifier, &path)? {
                        return Ok(Some(found));
                    }
                }
            }
            if current.pop().is_none() {
                return Ok(None);
            }
        }
    }

    fn resolve_with_config_at(
        &self,
        specifier: &str,
        path: &[String],
    ) -> Result<Option<String>, S::Error> {
        let Some(config) = self.module_paths(path, &HashSet::new())? else {
            return Ok(None);
        };
        if let Some(found) = self.resolve_with(specifier, &config)? {
            return Ok(Some(found));
        }
        for reference in &config.references {
            if self.budget.is_spent() {
                return Ok(None);
            }
            let file = self.config_file(reference)?;
            let Some(referenced) = self.module_paths(&file, &HashSet::new())? else {
                continue;
            };
            if let Some(found) = self.resolve_with(specifier, &referenced)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    fn resolve_with(
        &self,
        specifier: &str,
        config: &ModulePaths,
    ) -> Result<Option<String>, S::Error> {
        if let Some((patterns, directory)) = &config.paths
            && let Some((targets, wildcard)) = match_pattern(specifier, patterns)
        {
            let base = config.base_url.as_ref().unwrap_or(directory);
            for target in targets {
                if target.starts_with('/') {
                    continue;
                }
                let Some(path) = flatten(base, &target.replace('*', wildcard)) else {
                    continue;
                };
                if let Some(found) = self.probe(&path)? {
                    return Ok(Some(found));
                }
            }
        }
        let Some(base_url) = &config.base_url else {
            return Ok(None);
        };
        match flatten(base_url, specifier) {
            Some(path) => self.probe(&path),
            None => Ok(None),
        }
    }

    /// Reads one config and, for whatever it leaves unset, the configs it extends. `None` when
    /// the file is not text or does not parse; a failed read is an error. Only a relative
    /// `extends` is followed: a config shipped in a package is not in the tree. `visited`
    /// holds the configs further down the `extends` chain, so a cycle ends; `None` too once
    /// the budget is spent.
    fn module_paths(
        &self,
        path: &[String],
        visited: &HashSet<Vec<String>>,
    ) -> Result<Option<ModulePaths>, S::Error> {
        if !self.budget.spend() {
            return Ok(None);
        }
        let Some(bytes) = self.source.read(&path.join("/"))? else {
            return Ok(None);
        };
        let Some(text) = std::str::from_utf8(&bytes).ok() else {
            return Ok(None);
        };
        let Some(mut root) = parse_object(text) else {
            return Ok(None);
        };
        let directory = &path[..path.len().saturating_sub(1)];
        let mut result = ModulePaths::default();
        if let Some(mut options) = root.take_object("compilerOptions") {
            if let Some(base_url) = options.get_string("baseUrl") {
                result.base_url = flatten(directory, base_url);
            }
            if let Some(patterns) = options.take_object("paths").and_then(string_lists) {
                result.paths = Some((patterns, directory.to_vec()));
            }
        }
        result.references = references(&root, directory);
        self.inherit(&mut result, &root, path, visited)?;
        Ok(Some(result))
    }

    /// Fills what `result` leaves unset from the configs the one at `path` extends, the last
    /// entry first. A config already on the chain, or named twice, is read once at most.
    fn inherit(
        &self,
        result: &mut ModulePaths,
        root: &JsonObject<'_>,
        path: &[String],
        visited: &HashSet<Vec<String>>,
    ) -> Result<(), S::Error> {
        let mut chain = visited.clone();
        chain.insert(path.to_vec());
        if chain.len() > EXTENDS_DEPTH || (result.base_url.is_some() && result.paths.is_some()) {
            return Ok(());
        }
        let directory = &path[..path.len().saturating_sub(1)];
        let mut seen = chain.clone();
        for parent in extended_by(root).iter().rev() {
            if !parent.starts_with('.') {
                continue;
            }
            if self.budget.is_spent() {
                return Ok(());
            }
            let Some(file) = self.unseen_config(parent, directory, &mut seen)? else {
                continue;
            };
            if let Some(inherited) = self.module_paths(&file, &chain)? {
                result.fill_unset(inherited);
            }
        }
        Ok(())
    }

    /// The config file an `extends` entry names, or `None` when it leaves the repository or
    /// names a config already in `seen`, which then holds it.
    fn unseen_config(
        &self,
        parent: &str,
        directory: &[String],
        seen: &mut HashSet<Vec<String>>,
    ) -> Result<Option<Vec<String>>, S::Error> {
        let Some(target) = flatten(directory, parent) else {
            return Ok(None);
        };
        if !seen.insert(target.clone()) {
            return Ok(None);
        }
        let file = self.config_file(&target)?;
        if file == target || seen.insert(file.clone()) {
            return Ok(Some(file));
        }
        Ok(None)
    }

    /// The config file a reference or an `extends` names: the path itself, the path with
    /// `.json` when that was left out, or the `tsconfig.json` of the directory it names.
    fn config_file(&self, target: &[String]) -> Result<Vec<String>, S::Error> {
        let Some((name, parent)) = target.split_last() else {
            return Ok(vec!["tsconfig.json".to_owned()]);
        };
        let Some(siblings) = self.list(parent)? else {
            return Ok(target.to_vec());
        };
        if is_file(name, &siblings) {
            return Ok(target.to_vec());
        }
        let with_json = format!("{name}.json");
        if is_file(&with_json, &siblings) {
            let mut file = parent.to_vec();
            file.push(with_json);
            return Ok(file);
        }
        let mut file = target.to_vec();
        file.push("tsconfig.json".to_owned());
        Ok(file)
    }
}

/// The `/`-separated components of `path`, empty ones dropped.
fn components(path: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for part in path.split('/') {
        if !part.is_empty() {
            parts.push(part.to_owned());
        }
    }
    parts
}

/// `name` appended to `directory`, joined with `/`.
fn joined(directory: &[String], name: &str) -> String {
    if directory.is_empty() {
        return name.to_owned();
    }
    format!("{}/{name}", directory.join("/"))
}

/// What `specifier` names when it is relative to the importing file, `None` when it is not.
/// `.`, `..` and a trailing slash name a directory, never a file beside it.
fn relative_kind(specifier: &str) -> Option<RelativeKind> {
    let last = specifier.rsplit('/').next().unwrap_or_default();
    if last == "." || last == ".." {
        return Some(RelativeKind::Directory);
    }
    if !specifier.starts_with("./") && !specifier.starts_with("../") {
        return None;
    }
    if last.is_empty() {
        Some(RelativeKind::Directory)
    } else {
        Some(RelativeKind::File)
    }
}

/// The file names `name` may stand for, in the order they are tried: itself, itself with each
/// source extension, then the sources of its compiled extension.
fn file_candidates(name: &str) -> Vec<String> {
    let mut candidates = vec![name.to_owned()];
    for extension in EXTENSIONS {
        candidates.push(format!("{name}{extension}"));
    }
    for (compiled, sources) in SOURCE_EXTENSIONS {
        let Some(stem) = name.strip_suffix(compiled) else {
            continue;
        };
        for source in sources {
            candidates.push(format!("{stem}{source}"));
        }
    }
    candidates
}

fn is_file(name: &str, entries: &[TreeEntry]) -> bool {
    entries.iter().any(|entry| {
        entry.name == name.as_bytes()
            && match entry.kind {
                EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink => true,
                EntryKind::Tree | EntryKind::Submodule => false,
            }
    })
}

/// The `paths` entry a specifier falls under and the text its `*` stands for. An exact
/// pattern wins; among wildcard patterns the longest prefix does, as in TypeScript. A tie goes
/// to the longer suffix, so the answer never depends on the order the object's keys come back
/// in. Lengths count characters, as Swift's `String.count` did.
fn match_pattern<'p, 's>(
    specifier: &'s str,
    patterns: &'p [(String, Vec<String>)],
) -> Option<(&'p [String], &'s str)> {
    for (pattern, targets) in patterns {
        if pattern == specifier {
            return Some((targets, ""));
        }
    }
    let mut best: Option<(&'p [String], &'s str, usize, usize)> = None;
    for (pattern, targets) in patterns {
        let Some((prefix, suffix)) = pattern.split_once('*') else {
            continue;
        };
        if specifier.len() < prefix.len() + suffix.len()
            || !specifier.starts_with(prefix)
            || !specifier.ends_with(suffix)
        {
            continue;
        }
        let (prefix_count, suffix_count) = (prefix.chars().count(), suffix.chars().count());
        if let Some((_, _, best_prefix, best_suffix)) = best
            && !(prefix_count > best_prefix
                || (prefix_count == best_prefix && suffix_count > best_suffix))
        {
            continue;
        }
        let wildcard = &specifier[prefix.len()..specifier.len() - suffix.len()];
        best = Some((targets, wildcard, prefix_count, suffix_count));
    }
    best.map(|(targets, wildcard, _, _)| (targets, wildcard))
}

/// Parses a config as JSON5-ish JSONC (comments, trailing commas, single quotes, bare keys),
/// as `JSONSerialization`'s `.json5Allowed` read it. `None` unless the top level is an object.
fn parse_object(text: &str) -> Option<JsonObject<'_>> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let options = ParseOptions {
        allow_missing_commas: false,
        ..ParseOptions::default()
    };
    match parse_to_value(text, &options) {
        Ok(Some(JsonValue::Object(root))) => Some(root),
        Ok(
            Some(
                JsonValue::String(_)
                | JsonValue::Number(_)
                | JsonValue::Boolean(_)
                | JsonValue::Array(_)
                | JsonValue::Null,
            )
            | None,
        )
        | Err(_) => None,
    }
}

/// An object whose every value is a list of strings, as `[String: [String]]`; `None` when any
/// value is something else, as Swift's cast refused the whole map.
fn string_lists(object: JsonObject<'_>) -> Option<Vec<(String, Vec<String>)>> {
    let mut lists = Vec::new();
    for (key, value) in object {
        let JsonValue::Array(items) = value else {
            return None;
        };
        let mut strings = Vec::new();
        for item in items {
            let JsonValue::String(text) = item else {
                return None;
            };
            strings.push(text.into_owned());
        }
        lists.push((key.into_owned(), strings));
    }
    Some(lists)
}

/// The configs a solution-style config hands its files to, as repository paths. Empty unless
/// `references` is a list of objects.
fn references(root: &JsonObject<'_>, directory: &[String]) -> Vec<Vec<String>> {
    let Some(entries) = root.get_array("references") else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries.iter() {
        let JsonValue::Object(reference) = entry else {
            return Vec::new();
        };
        if let Some(path) = reference
            .get_string("path")
            .and_then(|path| flatten(directory, path))
        {
            paths.push(path);
        }
    }
    paths
}

/// The `extends` entries of a config: one string or a list of strings.
fn extended_by(root: &JsonObject<'_>) -> Vec<String> {
    match root.get("extends") {
        Some(JsonValue::String(parent)) => vec![parent.to_string()],
        Some(JsonValue::Array(list)) => {
            let mut parents = Vec::new();
            for item in list.iter() {
                let JsonValue::String(parent) = item else {
                    return Vec::new();
                };
                parents.push(parent.to_string());
            }
            parents
        }
        Some(
            JsonValue::Number(_) | JsonValue::Boolean(_) | JsonValue::Object(_) | JsonValue::Null,
        )
        | None => Vec::new(),
    }
}

/// An [`ImportSource`] over a committed tree: one tree object per directory on the way to a
/// path, each read once per source, and config blobs read up to [`CONFIG_SIZE_LIMIT`].
pub struct TreeImportSource<'r> {
    repository: &'r Repository,
    root: Oid,
    trees: RefCell<HashMap<Oid, Vec<TreeEntry>>>,
}

impl<'r> TreeImportSource<'r> {
    /// A source reading the tree `root` of `repository`.
    #[must_use]
    pub fn new(repository: &'r Repository, root: Oid) -> Self {
        Self {
            repository,
            root,
            trees: RefCell::new(HashMap::new()),
        }
    }

    fn entries(&self, tree: Oid) -> rc_git_core::Result<Vec<TreeEntry>> {
        if let Some(cached) = self.trees.borrow().get(&tree) {
            return Ok(cached.clone());
        }
        let entries = self.repository.tree_entries(tree)?;
        self.trees.borrow_mut().insert(tree, entries.clone());
        Ok(entries)
    }
}

impl ImportSource for TreeImportSource<'_> {
    type Error = rc_git_core::Error;

    fn list(&self, directory: &str) -> rc_git_core::Result<Option<Vec<TreeEntry>>> {
        let mut current = self.root;
        for component in directory.split('/').filter(|part| !part.is_empty()) {
            let entries = self.entries(current)?;
            let Some(child) = entries
                .iter()
                .find(|entry| entry.name == component.as_bytes() && entry.kind == EntryKind::Tree)
            else {
                return Ok(None);
            };
            current = child.oid;
        }
        self.entries(current).map(Some)
    }

    fn read(&self, path: &str) -> rc_git_core::Result<Option<Vec<u8>>> {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let Some(entries) = self.list(parent)? else {
            return Ok(None);
        };
        let Some(entry) = entries.iter().find(|entry| entry.name == name.as_bytes()) else {
            return Ok(None);
        };
        match self.repository.read_blob(entry.oid, CONFIG_SIZE_LIMIT)? {
            BlobContent::Text { data, .. } => Ok(Some(data)),
            BlobContent::Binary { .. } | BlobContent::TooLarge { .. } => Ok(None),
        }
    }
}
