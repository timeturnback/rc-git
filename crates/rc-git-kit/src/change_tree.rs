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

/// One row a change list draws: a node, how deep it sits, and whether a folder shows its
/// children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleRow<'a, E> {
    /// The node.
    pub node: &'a ChangeNode<E>,
    /// How many folders it sits in; 0 at the top.
    pub depth: usize,
    /// Whether a folder row is open; `false` on a file row.
    pub expanded: bool,
}

/// The rows a list draws for `nodes`, top to bottom: every folder open except those whose
/// [`ChangeNode::key`] is in `collapsed`, whose children are left out. A list keeps the keys a
/// user closed, never the open ones, so a new commit's folders all open, as Swift's did.
#[must_use]
pub fn visible_rows<'a, E, S: std::hash::BuildHasher>(
    nodes: &'a [ChangeNode<E>],
    collapsed: &std::collections::HashSet<String, S>,
) -> Vec<VisibleRow<'a, E>> {
    let mut out = Vec::new();
    let mut stack: Vec<(&ChangeNode<E>, usize)> =
        nodes.iter().rev().map(|node| (node, 0)).collect();
    while let Some((node, depth)) = stack.pop() {
        let expanded = node.is_folder() && !collapsed.contains(&node.key());
        out.push(VisibleRow {
            node,
            depth,
            expanded,
        });
        if expanded {
            for child in node.children.iter().rev() {
                stack.push((child, depth + 1));
            }
        }
    }
    out
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

#[cfg(test)]
mod visible_tests {
    use std::collections::HashSet;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Path(&'static str);

    impl ChangeListEntry for Path {
        fn path(&self) -> Cow<'_, str> {
            Cow::Borrowed(self.0)
        }
    }

    fn labels(rows: &[VisibleRow<'_, Path>]) -> Vec<String> {
        rows.iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), row.node.name))
            .collect()
    }

    #[test]
    fn every_folder_opens_until_a_user_closes_it() {
        let entries = [
            Path("a/x.rs"),
            Path("a/y.rs"),
            Path("b/c/z.rs"),
            Path("top.rs"),
        ];
        let nodes = nodes(&entries, ChangeGrouping::Folders);
        let open = visible_rows(&nodes, &HashSet::new());
        assert_eq!(
            labels(&open),
            ["a", "  x.rs", "  y.rs", "b/c", "  z.rs", "top.rs"]
        );
        assert!(open[0].expanded);
        assert!(!open[1].expanded, "a file row is never expanded");
        let closed = visible_rows(&nodes, &HashSet::from(["a/".to_owned()]));
        assert_eq!(labels(&closed), ["a", "b/c", "  z.rs", "top.rs"]);
        assert!(!closed[0].expanded);
    }

    #[test]
    fn a_file_key_never_closes_a_folder_of_the_same_path() {
        let entries = [Path("a"), Path("a/x.rs")];
        let nodes = nodes(&entries, ChangeGrouping::Folders);
        let rows = visible_rows(&nodes, &HashSet::from(["a".to_owned()]));
        assert_eq!(labels(&rows), ["a", "  x.rs", "a"]);
    }

    #[test]
    fn a_flat_list_is_its_rows() {
        let entries = [Path("a/x.rs"), Path("b.rs")];
        let nodes = nodes(&entries, ChangeGrouping::Flat);
        assert_eq!(
            labels(&visible_rows(&nodes, &HashSet::new())),
            ["a/x.rs", "b.rs"]
        );
    }
}
