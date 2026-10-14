//! Rewrites a tree by a set of path edits, recursing only into the directories an edit
//! touches. Unaffected siblings keep their object ids, so the cost scales with the change,
//! never with repository size. Git never stores empty trees: a directory whose last entry
//! is removed disappears from its parent.

use std::collections::{BTreeMap, HashMap};

use gix::ObjectId;
use gix::bstr::BString;
use gix::objs::tree::{Entry, EntryKind};

use super::write::Writer;
use super::{ActionError, ActionResult};

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

/// Rewrites the root tree `base` with `edits`, writing each rebuilt tree object, and returns
/// the new root; `None` when everything was removed. `base` is `None` for a repository with
/// no commit yet.
///
/// # Errors
/// [`ActionError::Git`] when an edit lies inside a submodule (git keeps those in the
/// submodule's own repository, never as files of the superproject) and when a tree cannot be
/// read or written.
pub(super) fn rewrite(
    writer: &Writer<'_>,
    base: Option<ObjectId>,
    edits: &BTreeMap<BString, Node>,
) -> ActionResult<Option<ObjectId>> {
    rewrite_dir(writer, base, "", edits)
}

/// [`rewrite`] for the directory at `dir` (`""` for the root, otherwise ending in `/`).
fn rewrite_dir(
    writer: &Writer<'_>,
    base: Option<ObjectId>,
    dir: &str,
    edits: &BTreeMap<BString, Node>,
) -> ActionResult<Option<ObjectId>> {
    let mut entries: HashMap<BString, Entry> = match base {
        Some(id) => decode_entries(writer.repo(), id)?,
        None => HashMap::new(),
    };
    for (name, node) in edits {
        match node {
            Node::Leaf(PathEdit::Remove) => {
                entries.remove(name);
            }
            Node::Leaf(PathEdit::Put { oid, kind }) => {
                entries.insert(name.clone(), entry((*kind).into(), name.clone(), *oid));
            }
            Node::Dir(children) => {
                let path = format!("{dir}{name}/");
                let existing = entries.remove(name);
                if existing
                    .as_ref()
                    .is_some_and(|entry| entry.mode.is_commit())
                {
                    return Err(inside_submodule(&path, children));
                }
                let sub_base = existing
                    .filter(|entry| entry.mode.is_tree())
                    .map(|entry| entry.oid);
                if let Some(oid) = rewrite_dir(writer, sub_base, &path, children)? {
                    entries.insert(
                        name.clone(),
                        entry(EntryKind::Tree.into(), name.clone(), oid),
                    );
                }
            }
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let mut entries: Vec<Entry> = entries.into_values().collect();
    entries.sort();
    let tree = gix::objs::Tree { entries };
    writer.write_object(OP_BUILD, "tree", &tree).map(Some)
}

/// The refusal for an edit below the submodule at `dir` (ending in `/`), naming the first
/// selected path inside it.
fn inside_submodule(dir: &str, children: &BTreeMap<BString, Node>) -> ActionError {
    let mut path = dir.to_owned();
    let mut level = children;
    while let Some((name, node)) = level.iter().next() {
        path.push_str(&name.to_string());
        match node {
            Node::Leaf(_) => break,
            Node::Dir(grandchildren) => {
                path.push('/');
                level = grandchildren;
            }
        }
    }
    let submodule = dir.trim_end_matches('/');
    ActionError::git(
        OP_BUILD,
        path,
        format!("is inside submodule '{submodule}'; commit it in the submodule"),
    )
}

/// The tree entry `filename` pointing at `oid` with `mode`.
fn entry(mode: gix::objs::tree::EntryMode, filename: BString, oid: ObjectId) -> Entry {
    Entry {
        mode,
        filename,
        oid,
    }
}

/// The entries of `tree`, keyed by name (unique within a valid tree), so each edit is one
/// keyed lookup rather than a scan of the whole directory.
fn decode_entries(repo: &gix::Repository, tree: ObjectId) -> ActionResult<HashMap<BString, Entry>> {
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
        .map(|decoded| {
            let name = decoded.filename.to_owned();
            (
                name.clone(),
                entry(decoded.mode, name, decoded.oid.to_owned()),
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Policy};

    fn oid(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex.as_bytes()).unwrap()
    }

    #[test]
    fn rewriting_a_tree_beside_a_dotted_name_matches_git_mktree() {
        let fx = rc_git_testkit::Fixture::standard();
        let x = fx.hash_blob(b"x\n");
        let y = fx.hash_blob(b"y\n");
        let old = fx.hash_blob(b"old\n");
        let new = fx.hash_blob(b"new\n");
        let inner = fx.mktree(format!("100644 blob {x}\tx\n").as_bytes());
        let base = fx.mktree(
            format!("040000 tree {inner}\ta\n100644 blob {old}\ta.b\n100644 blob {old}\ta0\n")
                .as_bytes(),
        );

        let mut edits = BTreeMap::new();
        let put = |hex: &str| PathEdit::Put {
            oid: oid(hex),
            kind: EntryKind::Blob,
        };
        insert(&mut edits, "a/y", put(&y)).unwrap();
        insert(&mut edits, "a.b", put(&new)).unwrap();
        insert(&mut edits, "a0", PathEdit::Remove).unwrap();
        insert(&mut edits, "a-c", put(&new)).unwrap();

        let repo = gix::open_opts(fx.path(), gix::open::Options::isolated()).unwrap();
        let proof = Policy::default().authorize(&Action::Fetch, None).unwrap();
        let writer = Writer::new(&repo, &proof);
        let rebuilt = rewrite(&writer, Some(oid(&base)), &edits).unwrap().unwrap();

        let inner = fx.mktree(format!("100644 blob {x}\tx\n100644 blob {y}\ty\n").as_bytes());
        let expected = fx.mktree(
            format!("100644 blob {new}\ta-c\n040000 tree {inner}\ta\n100644 blob {new}\ta.b\n")
                .as_bytes(),
        );
        assert_eq!(rebuilt, oid(&expected));
    }
}
