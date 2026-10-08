//! Rewrites a tree by a set of path edits, recursing only into the directories an edit
//! touches. Unaffected siblings keep their object ids, so the cost scales with the change,
//! never with repository size. Git never stores empty trees: a directory whose last entry
//! is removed disappears from its parent.

use std::collections::BTreeMap;

use gix::ObjectId;
use gix::bstr::BString;
use gix::objs::tree::{Entry, EntryKind};

use super::{ActionError, ActionResult, git_err};

/// The operation a path-insertion refusal names.
const OP_BUILD: &str = "build tree";
/// Why [`insert`] refuses a path.
const OVERLAP: &str = "the selected paths overlap or a path is empty";

/// What happens to one path.
pub(super) enum PathEdit {
    /// The path points at `oid` with `kind` afterwards, added or replaced.
    Put {
        /// The blob to store.
        oid: ObjectId,
        /// Blob, executable blob, or symlink.
        kind: EntryKind,
    },
    /// The path is gone afterwards.
    Remove,
}

/// One level of the edit trie.
pub(super) enum Node {
    Leaf(PathEdit),
    Dir(BTreeMap<BString, Node>),
}

/// Files a `PathEdit` for `path` into the trie rooted at `root`. A removal beneath a path
/// that is replaced by a file is already implied by that replacement — rewriting the parent
/// drops the whole subtree — so it is accepted in either order and folded into it, which lets
/// both sides of a directory-to-file change be selected together.
///
/// # Errors
/// [`ActionError::Git`] for a path git refuses to store (see
/// [`validate_path`](super::workfile::validate_path)) and for overlapping paths.
pub(super) fn insert(
    root: &mut BTreeMap<BString, Node>,
    path: &str,
    edit: PathEdit,
) -> ActionResult<()> {
    let removal = match &edit {
        PathEdit::Put { kind, .. } => {
            super::workfile::validate_path("build tree", path, *kind == EntryKind::Link)?;
            false
        }
        PathEdit::Remove => {
            super::workfile::validate_path("build tree", path, false)?;
            true
        }
    };
    let overlap = || ActionError::git(OP_BUILD, path, OVERLAP);
    let mut segments = path.split('/').filter(|s| !s.is_empty()).peekable();
    let mut current = root;
    while let Some(segment) = segments.next() {
        let name = BString::from(segment);
        if segments.peek().is_none() {
            return match current.get(&name) {
                None => {
                    current.insert(name, Node::Leaf(edit));
                    Ok(())
                }
                Some(Node::Dir(children)) if !removal && only_removals(children) => {
                    current.insert(name, Node::Leaf(edit));
                    Ok(())
                }
                Some(_) => Err(overlap()),
            };
        }
        let child = current
            .entry(name)
            .or_insert_with(|| Node::Dir(BTreeMap::new()));
        current = match child {
            Node::Dir(children) => children,
            Node::Leaf(PathEdit::Put { .. }) if removal => return Ok(()),
            Node::Leaf(_) => return Err(overlap()),
        };
    }
    Err(overlap())
}

/// Whether every edit below `children` is a removal.
fn only_removals(children: &BTreeMap<BString, Node>) -> bool {
    children.values().all(|node| match node {
        Node::Leaf(PathEdit::Remove) => true,
        Node::Leaf(PathEdit::Put { .. }) => false,
        Node::Dir(grandchildren) => only_removals(grandchildren),
    })
}

/// Rewrites the tree at `base` with `edits`, writing each rebuilt tree object, and returns
/// the new root; `None` when everything was removed. `base` is `None` for a directory that
/// does not exist yet.
pub(super) fn rewrite(
    repo: &gix::Repository,
    base: Option<ObjectId>,
    edits: &BTreeMap<BString, Node>,
) -> ActionResult<Option<ObjectId>> {
    const OP: &str = "build tree";
    let mut entries: Vec<Entry> = match base {
        Some(id) => decode_entries(repo, id)?,
        None => Vec::new(),
    };
    for (name, node) in edits {
        match node {
            Node::Leaf(PathEdit::Remove) => {
                entries.retain(|entry| entry.filename != *name);
            }
            Node::Leaf(PathEdit::Put { oid, kind }) => {
                entries.retain(|entry| entry.filename != *name);
                entries.push(Entry {
                    mode: (*kind).into(),
                    filename: name.clone(),
                    oid: *oid,
                });
            }
            Node::Dir(children) => {
                let sub_base = entries
                    .iter()
                    .find(|entry| entry.filename == *name && entry.mode.is_tree())
                    .map(|entry| entry.oid);
                let rebuilt = rewrite(repo, sub_base, children)?;
                entries.retain(|entry| entry.filename != *name);
                if let Some(oid) = rebuilt {
                    entries.push(Entry {
                        mode: EntryKind::Tree.into(),
                        filename: name.clone(),
                        oid,
                    });
                }
            }
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    entries.sort();
    let tree = gix::objs::Tree { entries };
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    let oid = repo.write_object(&tree).map_err(git_err(OP, "tree"))?;
    Ok(Some(oid.detach()))
}

fn decode_entries(repo: &gix::Repository, tree: ObjectId) -> ActionResult<Vec<Entry>> {
    const OP: &str = "read tree";
    let tree = repo
        .find_tree(tree)
        .map_err(|source| ActionError::git(OP, tree.to_string(), source))?;
    let decoded = tree
        .decode()
        .map_err(|source| ActionError::git(OP, tree.id.to_string(), source))?;
    Ok(decoded
        .entries
        .iter()
        .map(|entry| Entry {
            mode: entry.mode,
            filename: entry.filename.to_owned(),
            oid: entry.oid.to_owned(),
        })
        .collect())
}
