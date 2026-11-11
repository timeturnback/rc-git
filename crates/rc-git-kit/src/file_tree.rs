//! The Files tree's state (`TreeViewController.swift`'s nodes): every row read so far, which
//! folders are expanded, and what a folder shows while its children are not on screen yet.
//!
//! Children load lazily, one read per first expand: [`FileTree::expand`] hands back the read
//! to run and [`FileTree::finish`] files its answer. Hidden rows are kept; [`FileTree::view`]
//! asks the caller's filter at display time, so the ⇧⌘. toggle re-filters without a read.
//!
//! A node's id is its index path (`"0/3/2"`), not its repository path: a lossy name can repeat
//! a sibling's display spelling, and an index path names one node whatever its name.

use rc_git_core::bstr::ByteSlice;
use rc_git_core::{EntryKind, Oid};

use crate::models::BrowseRow;

/// What an expanded folder shows while its children are being read.
pub const LOADING_TEXT: &str = "Loading\u{2026}";
/// What an expanded folder shows when its children could not be read.
pub const UNREADABLE_TEXT: &str = "Could not read this tree";
/// What an expanded folder shows when it has no child on screen.
pub const EMPTY_TEXT: &str = "No files";

/// A folder's children, as far as they have been read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Children {
    /// Never expanded, so never read.
    NotLoaded,
    /// The read is running.
    Loading,
    /// The read answered.
    Loaded(Vec<Node>),
    /// The read failed with this message; expanding again retries.
    Failed(String),
}

/// One row of the tree and, for a folder, its children.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    row: BrowseRow,
    path: String,
    path_is_exact: bool,
    expanded: bool,
    children: Children,
}

impl Node {
    fn new(row: BrowseRow, parent: Option<(&str, bool)>) -> Self {
        let name = row.entry.name_lossy();
        let exact = row.entry.name.to_str().is_ok();
        let (path, path_is_exact) = match parent {
            Some((parent, parent_exact)) => (format!("{parent}/{name}"), parent_exact && exact),
            None => (name.into_owned(), exact),
        };
        Self {
            row,
            path,
            path_is_exact,
            expanded: false,
            children: Children::NotLoaded,
        }
    }

    fn is_folder(&self) -> bool {
        self.row.entry.kind == EntryKind::Tree
    }
}

/// The read a first expand needs: the folder's tree for a committed tree, or its directory on
/// disk when the tree browses the working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildLoad {
    /// The folder's node id, which [`FileTree::finish`] takes back.
    pub id: String,
    /// The folder's tree object.
    pub tree: Oid,
    /// The folder's repository-relative path.
    pub path: String,
    /// The root generation the read was asked under; a newer root drops its answer.
    pub generation: u64,
}

/// A node the caller acts on: its row, path, and whether every component of the path is the
/// stored name byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    /// The row.
    pub row: BrowseRow,
    /// The repository-relative path.
    pub path: String,
    /// Whether the path names this file alone.
    pub path_is_exact: bool,
}

/// One row as the view draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewNode {
    /// The node id; a placeholder's is its folder's id plus `/-`.
    pub id: String,
    /// The label.
    pub label: String,
    /// What the row is.
    pub kind: ViewKind,
}

/// What a [`ViewNode`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewKind {
    /// A folder; collapsed folders carry one placeholder child, so the view offers to
    /// expand them before anything has been read.
    Folder {
        /// Whether its children are on screen.
        expanded: bool,
        /// The rows under it.
        children: Vec<ViewNode>,
    },
    /// A file, symlink or submodule, with its row.
    Leaf(Box<BrowseRow>),
    /// The text standing in for a folder's children.
    Placeholder,
}

/// The tree's nodes and the generation of the root they hang from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTree {
    roots: Vec<Node>,
    generation: u64,
}

impl FileTree {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces every node with the root's rows; reads still running for the old root are
    /// dropped when they land.
    pub fn set_root(&mut self, rows: Vec<BrowseRow>) {
        self.generation += 1;
        self.roots = rows.into_iter().map(|row| Node::new(row, None)).collect();
    }

    /// Empties the tree, dropping any read still running.
    pub fn clear(&mut self) {
        self.set_root(Vec::new());
    }

    /// The generation reads must match to land.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// How many nodes have been read, at any depth.
    #[must_use]
    pub fn node_count(&self) -> usize {
        fn count(nodes: &[Node]) -> usize {
            let mut total = nodes.len();
            for node in nodes {
                if let Children::Loaded(children) = &node.children {
                    total += count(children);
                }
            }
            total
        }
        count(&self.roots)
    }

    /// The node `id` names.
    #[must_use]
    pub fn info(&self, id: &str) -> Option<NodeInfo> {
        let node = self.node(id)?;
        Some(NodeInfo {
            row: node.row.clone(),
            path: node.path.clone(),
            path_is_exact: node.path_is_exact,
        })
    }

    /// Expands folder `id`. The first expand, and one after a failed read, starts its read:
    /// the returned load is the caller's to run. `None` when there is nothing to read.
    pub fn expand(&mut self, id: &str) -> Option<ChildLoad> {
        let generation = self.generation;
        let node = self.node_mut(id)?;
        if !node.is_folder() {
            return None;
        }
        node.expanded = true;
        match node.children {
            Children::NotLoaded | Children::Failed(_) => {
                node.children = Children::Loading;
                Some(ChildLoad {
                    id: id.to_owned(),
                    tree: node.row.entry.oid,
                    path: node.path.clone(),
                    generation,
                })
            }
            Children::Loading | Children::Loaded(_) => None,
        }
    }

    /// Collapses folder `id`; its children stay read.
    pub fn collapse(&mut self, id: &str) {
        if let Some(node) = self.node_mut(id) {
            node.expanded = false;
        }
    }

    /// Files the answer of `load`. `false`, changing nothing, when the root changed since.
    pub fn finish(&mut self, load: &ChildLoad, result: Result<Vec<BrowseRow>, String>) -> bool {
        if load.generation != self.generation {
            return false;
        }
        let Some(node) = self.node_mut(&load.id) else {
            return false;
        };
        let parent = (node.path.clone(), node.path_is_exact);
        node.children = match result {
            Ok(rows) => Children::Loaded(
                rows.into_iter()
                    .map(|row| Node::new(row, Some((&parent.0, parent.1))))
                    .collect(),
            ),
            Err(message) => Children::Failed(message),
        };
        true
    }

    /// The rows on screen, with `visible` deciding which read rows show.
    #[must_use]
    pub fn view(&self, visible: &dyn Fn(&BrowseRow) -> bool) -> Vec<ViewNode> {
        view_nodes(&self.roots, "", visible)
    }

    fn node(&self, id: &str) -> Option<&Node> {
        let mut nodes = &self.roots;
        let mut found = None;
        for index in parse_id(id)? {
            let node = nodes.get(index)?;
            nodes = match &node.children {
                Children::Loaded(children) => children,
                Children::NotLoaded | Children::Loading | Children::Failed(_) => &EMPTY,
            };
            found = Some(node);
        }
        found
    }

    fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        let mut indices = parse_id(id)?.into_iter();
        let mut node = self.roots.get_mut(indices.next()?)?;
        for index in indices {
            let Children::Loaded(children) = &mut node.children else {
                return None;
            };
            node = children.get_mut(index)?;
        }
        Some(node)
    }
}

static EMPTY: Vec<Node> = Vec::new();

fn parse_id(id: &str) -> Option<Vec<usize>> {
    let mut indices = Vec::new();
    for part in id.split('/') {
        indices.push(part.parse().ok()?);
    }
    Some(indices)
}

fn child_id(parent: &str, index: usize) -> String {
    if parent.is_empty() {
        index.to_string()
    } else {
        format!("{parent}/{index}")
    }
}

fn view_nodes(nodes: &[Node], parent: &str, visible: &dyn Fn(&BrowseRow) -> bool) -> Vec<ViewNode> {
    let mut out = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        if !visible(&node.row) {
            continue;
        }
        let id = child_id(parent, index);
        let label = node.row.entry.name_lossy().into_owned();
        let kind = if node.is_folder() {
            ViewKind::Folder {
                expanded: node.expanded,
                children: folder_children(node, &id, visible),
            }
        } else {
            ViewKind::Leaf(Box::new(node.row.clone()))
        };
        out.push(ViewNode { id, label, kind });
    }
    out
}

fn folder_children(node: &Node, id: &str, visible: &dyn Fn(&BrowseRow) -> bool) -> Vec<ViewNode> {
    let text = match &node.children {
        Children::Loaded(children) => {
            let shown = view_nodes(children, id, visible);
            if !shown.is_empty() {
                return shown;
            }
            EMPTY_TEXT
        }
        Children::Failed(_) => UNREADABLE_TEXT,
        Children::NotLoaded | Children::Loading => LOADING_TEXT,
    };
    vec![ViewNode {
        id: format!("{id}/-"),
        label: text.to_owned(),
        kind: ViewKind::Placeholder,
    }]
}
