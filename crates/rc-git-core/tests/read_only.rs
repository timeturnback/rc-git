//! Proves the library never writes to `.git`: every operation runs, then the directory is
//! compared byte for byte with a snapshot taken before.
#![expect(clippy::unwrap_used, reason = "test helpers panic on fixture failure")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rc_git_core::{DEFAULT_BLOB_LIMIT, Repository};
use rc_git_testkit::Fixture;

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let contents = std::fs::read(&path).unwrap();
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), contents);
            }
        }
    }
    files
}

#[test]
fn every_operation_leaves_git_dir_untouched() {
    let fx = Fixture::standard();
    let before = snapshot(&fx.git_dir());
    assert!(
        before.len() > 10,
        "snapshot should cover objects, refs, and config"
    );

    let repo = Repository::open(fx.path()).unwrap();
    repo.head().unwrap();
    let listing = repo.refs().unwrap();
    assert!(!listing.refs.is_empty());
    let tip = repo.resolve("main").unwrap();
    let root = repo.resolve_tree("v1.0").unwrap();
    assert_eq!(repo.tree_of(tip).unwrap(), root);
    for entry in repo.tree_entries(root).unwrap() {
        if entry.kind == rc_git_core::EntryKind::Tree {
            repo.tree_entries(entry.oid).unwrap();
        } else if entry.kind != rc_git_core::EntryKind::Submodule {
            repo.blob_size(entry.oid).unwrap();
            repo.read_blob(entry.oid, DEFAULT_BLOB_LIMIT).unwrap();
        }
    }
    repo.entry_at_path(root, "src/main.rs").unwrap();
    for commit in repo.log(tip, 0, 100).unwrap() {
        repo.commit(commit.oid).unwrap();
    }
    let old = repo.resolve("v0.9").unwrap();
    for change in repo.diff_trees(Some(old), tip).unwrap() {
        repo.diff_blobs(
            change.old,
            change.new,
            DEFAULT_BLOB_LIMIT,
            rc_git_core::DEFAULT_DIFF_CONTEXT,
        )
        .unwrap();
    }
    repo.blame(tip, "README.md").unwrap();
    let listing = repo.worktree_entries(root, "").unwrap();
    assert!(!listing.entries.is_empty());
    repo.worktree_entries(root, "src").unwrap();
    repo.worktree_file("README.md", DEFAULT_BLOB_LIMIT).unwrap();
    drop(repo);

    assert_eq!(snapshot(&fx.git_dir()), before);
}
