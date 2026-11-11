//! Port of `ChangeTree.swift`: how a change list, the working tree's Changes list or a History
//! commit's changed files, lays its paths out, and which files a Discard acts on.
//!
//! The grouping each list remembers between launches is stored by the app;
//! [`ChangeGrouping::as_str`] and [`ChangeGrouping::parse`] are the stored form.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};

use rc_git_core::bstr::ByteSlice;
use rc_git_core::{StatusEntry, TreeChange};

use crate::natural_sort;

/// What a change list row is built from: anything that names one repository path.
pub trait ChangeListEntry: Clone {
    /// Repository-relative path with `/` separators.
    fn path(&self) -> Cow<'_, str>;
}

impl ChangeListEntry for StatusEntry {
    fn path(&self) -> Cow<'_, str> {
        self.path.to_str_lossy()
    }
}

impl ChangeListEntry for TreeChange {
    fn path(&self) -> Cow<'_, str> {
        self.path.to_str_lossy()
    }
}

/// How a change list lays its paths out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeGrouping {
    /// One row per changed file, labeled with the whole path.
    Flat,
    /// Files nested under their folders.
    Folders,
}

impl ChangeGrouping {
    /// Every grouping, in the order a picker lists them.
    pub const ALL: [Self; 2] = [Self::Flat, Self::Folders];

    /// The stored form, the same string the Swift app wrote.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Folders => "folders",
        }
    }

    /// The grouping a stored string names; `None` for anything else, so the caller falls back
    /// to its list's default.
    #[must_use]
    pub fn parse(stored: &str) -> Option<Self> {
        match stored {
            "flat" => Some(Self::Flat),
            "folders" => Some(Self::Folders),
            _ => None,
        }
    }
}

/// One row of a change list: a changed file, or a folder holding changed files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeNode<E> {
    /// What the row shows: the whole path in `Flat`, the file or folder name in `Folders`.
    pub name: String,
    /// Repository-relative path with `/` separators; no trailing slash on a folder.
    pub path: String,
    /// The change itself; `None` on a folder row.
    pub entry: Option<E>,
    /// The rows nested under a folder: subfolders by name, then files in report order.
    pub children: Vec<ChangeNode<E>>,
}

impl<E> ChangeNode<E> {
    fn file(name: String, path: String, entry: E) -> Self {
        Self {
            name,
            path,
            entry: Some(entry),
            children: Vec::new(),
        }
    }

    /// Whether the row is a folder rather than a changed file.
    #[must_use]
    pub fn is_folder(&self) -> bool {
        self.entry.is_none()
    }

    /// Names this row uniquely in its list. A file and a folder can share `path` (a directory
    /// replaced by a file of the same name), so a folder's key ends in `/`.
    #[must_use]
    pub fn key(&self) -> String {
        if self.is_folder() {
            format!("{}/", self.path)
        } else {
            self.path.clone()
        }
    }

    /// Every changed path at or below this row, in row order. Walked on each read: only a
    /// click on a row or a Discard asks, never a render.
    #[must_use]
    pub fn file_paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        self.collect_paths(&mut paths);
        paths
    }

    fn collect_paths(&self, paths: &mut Vec<String>) {
        if self.entry.is_some() {
            paths.push(self.path.clone());
            return;
        }
        for child in &self.children {
            child.collect_paths(paths);
        }
    }
}

/// The rows for `entries` under `grouping`. `entries` is expected in path order, which is the
/// order a status report and a tree diff return.
#[must_use]
pub fn nodes<E: ChangeListEntry>(entries: &[E], grouping: ChangeGrouping) -> Vec<ChangeNode<E>> {
    match grouping {
        ChangeGrouping::Flat => entries
            .iter()
            .map(|entry| {
                let path = entry.path().into_owned();
                ChangeNode::file(path.clone(), path, entry.clone())
            })
            .collect(),
        ChangeGrouping::Folders => folders(entries),
    }
}

/// The files a Discard on these rows returns to `HEAD` or deletes, sorted, each once.
///
/// A right-clicked row outside the selection acts alone, as a context menu does everywhere
/// else; a clicked row inside it, or no clicked row, acts on the whole selection. A folder row
/// covers every file beneath it, which is what its tick box covers. Rows are told apart by
/// [`ChangeNode::key`], which is unique within one list.
#[must_use]
pub fn discard_paths<E>(
    selected: &[&ChangeNode<E>],
    clicked: Option<&ChangeNode<E>>,
) -> Vec<String> {
    let mut rows: Vec<&ChangeNode<E>> = selected.to_vec();
    if let Some(clicked) = clicked {
        let key = clicked.key();
        if !selected.iter().any(|row| row.key() == key) {
            rows = vec![clicked];
        }
    }
    let mut paths = BTreeSet::new();
    for row in rows {
        paths.extend(row.file_paths());
    }
    paths.into_iter().collect()
}

/// A folder's files, and the folders under it, while the tree is being built.
struct Level<E> {
    folders: HashMap<String, Level<E>>,
    files: Vec<E>,
}

impl<E> Level<E> {
    fn new() -> Self {
        Self {
            folders: HashMap::new(),
            files: Vec::new(),
        }
    }
}

fn folders<E: ChangeListEntry>(entries: &[E]) -> Vec<ChangeNode<E>> {
    let mut root = Level::new();
    for entry in entries {
        let path = entry.path();
        let mut components: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        if components.pop().is_none() {
            continue;
        }
        let mut level = &mut root;
        for component in components {
            level = level
                .folders
                .entry(component.to_owned())
                .or_insert_with(Level::new);
        }
        level.files.push(entry.clone());
    }
    rows(root, "")
}

/// The rows of one folder: its subfolders by name, then its own files in report order.
fn rows<E: ChangeListEntry>(level: Level<E>, path: &str) -> Vec<ChangeNode<E>> {
    let mut subfolders: Vec<(String, Level<E>)> = level.folders.into_iter().collect();
    subfolders.sort_by(|a, b| natural_sort::compare(&a.0, &b.0));
    let mut rows = Vec::new();
    for (name, child) in subfolders {
        let child_path = join(path, &name);
        rows.push(folder_row(name, child_path, child));
    }
    for entry in level.files {
        let path = entry.path().into_owned();
        rows.push(ChangeNode::file(
            last_component(&path).to_owned(),
            path,
            entry,
        ));
    }
    rows
}

/// One folder row. A folder with no files and a single subfolder merges into it, so a path
/// like `docs/solutions/ui-bugs` is one row rather than three nested ones. A folder with files
/// has a file row among its children, so one child that is a folder means no files.
fn folder_row<E: ChangeListEntry>(name: String, path: String, level: Level<E>) -> ChangeNode<E> {
    let mut children = rows(level, &path);
    if children.len() == 1 && children[0].is_folder() {
        let only = children.remove(0);
        return ChangeNode {
            name: format!("{name}/{}", only.name),
            path: only.path,
            entry: None,
            children: only.children,
        };
    }
    ChangeNode {
        name,
        path,
        entry: None,
        children,
    }
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}/{name}")
    }
}

fn last_component(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}
