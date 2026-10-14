//! Proves the library never writes: every operation runs, then the fixture, `.git` and the
//! working tree both, is compared with a snapshot taken before: every directory, every file's
//! bytes, every symlink's target, and every entry's permission bits.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rc_git_core::{DEFAULT_BLOB_LIMIT, Repository, ScanWindow, WorktreeStatus};
use rc_git_testkit::Fixture;

#[derive(Debug, PartialEq, Eq)]
enum Node {
    Dir { mode: u32 },
    File { mode: u32, contents: Vec<u8> },
    Symlink { target: PathBuf },
}

fn with_path(op: &str, path: &Path) -> impl FnOnce(io::Error) -> io::Error {
    let context = format!("snapshot {op} {}", path.display());
    move |e| io::Error::new(e.kind(), format!("{context}: {e}"))
}

fn read_node(path: &Path) -> io::Result<Node> {
    let meta = std::fs::symlink_metadata(path).map_err(with_path("lstat", path))?;
    let mode = meta.permissions().mode();
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path).map_err(with_path("read_link", path))?;
        Ok(Node::Symlink { target })
    } else if meta.is_dir() {
        Ok(Node::Dir { mode })
    } else {
        let contents = std::fs::read(path).map_err(with_path("read", path))?;
        Ok(Node::File { mode, contents })
    }
}

fn snapshot(root: &Path) -> io::Result<BTreeMap<PathBuf, Node>> {
    let mut nodes = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(with_path("read_dir", &dir))? {
            let path = entry.map_err(with_path("read_dir entry in", &dir))?.path();
            let node = read_node(&path)?;
            if matches!(node, Node::Dir { .. }) {
                pending.push(path.clone());
            }
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            nodes.insert(relative.to_path_buf(), node);
        }
    }
    Ok(nodes)
}

/// The standard fixture with packed objects, packed refs and a commit graph under a loose
/// stash, plus an ignore file, ignored and untracked files, and a modified tracked file, so
/// every read path (pack and loose, ignore stack, content hashing) runs under the snapshot.
fn packed_fixture_with_local_changes() -> Fixture {
    let fx = Fixture::standard();
    fx.git(&["repack", "-adq"]);
    fx.git(&["pack-refs", "--all"]);
    fx.git(&["commit-graph", "write", "--reachable"]);
    fx.write("alpha.txt", b"stashed\n");
    fx.git(&["stash", "push", "-q", "-m", "kept for reading"]);
    fx.write(".gitignore", b"*.tmp\nbuild/\n");
    fx.write("scratch.tmp", b"x");
    fx.write("build/out.o", b"x");
    fx.write("notes/todo.txt", b"x");
    fx.write("Zeta.txt", b"edited\n");
    fx
}

fn assert_unchanged(before: &BTreeMap<PathBuf, Node>, after: &BTreeMap<PathBuf, Node>) {
    let added: Vec<_> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .collect();
    let removed: Vec<_> = before
        .keys()
        .filter(|path| !after.contains_key(*path))
        .collect();
    let changed: Vec<_> = before
        .iter()
        .filter(|(path, node)| after.get(*path).is_some_and(|now| now != *node))
        .map(|(path, _)| path)
        .collect();
    assert!(
        added.is_empty() && removed.is_empty() && changed.is_empty(),
        "core wrote to the repository: added {added:?} removed {removed:?} changed {changed:?}"
    );
}

#[test]
fn every_operation_leaves_the_repository_untouched() {
    let fx = packed_fixture_with_local_changes();
    let before = snapshot(fx.path()).unwrap();
    assert!(
        before.len() > 10,
        "snapshot should cover objects, refs, and config"
    );

    let repo = Repository::open(fx.path()).unwrap();
    repo.head().unwrap();
    let listing = repo.refs().unwrap();
    assert_ne!(listing.refs.len(), 0);
    assert_eq!(repo.default_branch().unwrap().as_deref(), Some("main"));
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
        repo.commit_details(commit.oid).unwrap();
    }
    let window = ScanWindow {
        scan_skip: 0,
        limit: 100,
        max_scan: 1000,
    };
    repo.path_history(tip, "README.md", window).unwrap();
    let filter = rc_git_core::CommitFilter {
        message: Some("docs".to_owned()),
        ..rc_git_core::CommitFilter::default()
    };
    assert!(
        !repo
            .search_commits(tip, &filter, window)
            .unwrap()
            .commits
            .is_empty()
    );
    let scoped = rc_git_core::CommitFilter {
        path: Some("README.md".to_owned()),
        ..filter
    };
    repo.search_commits(tip, &scoped, window).unwrap();
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
    repo.diff_trees_without_renames(Some(old), tip).unwrap();
    repo.blame(tip, "README.md", DEFAULT_BLOB_LIMIT, Some(i64::MAX))
        .unwrap();
    let listing = repo.worktree_entries(Some(root), "").unwrap();
    assert_ne!(listing.entries.len(), 0);
    repo.worktree_entries(Some(root), "src").unwrap();
    repo.worktree_file("README.md", DEFAULT_BLOB_LIMIT).unwrap();
    let status = repo.worktree_status(Some(root), 100_000).unwrap();
    assert!(!status.truncated);
    assert!(
        status
            .entries
            .iter()
            .any(|entry| entry.status == WorktreeStatus::Untracked),
        "the fixture must hold an untracked file so the ignore stack is built: {:?}",
        status.entries
    );
    assert!(
        status
            .entries
            .iter()
            .any(|entry| entry.status == WorktreeStatus::Modified),
        "the fixture must hold a modified file so its content is hashed: {:?}",
        status.entries
    );
    repo.worktree_status(None, 100_000).unwrap();
    let files = repo.worktree_files(Some(root), 100_000).unwrap();
    assert!(!files.truncated);
    let readme = repo.entry_at_path(root, "README.md").unwrap().unwrap();
    repo.diff_worktree_file(Some(readme.oid), "README.md", DEFAULT_BLOB_LIMIT, 3)
        .unwrap();
    repo.diff_worktree_file(None, "README.md", DEFAULT_BLOB_LIMIT, 3)
        .unwrap();
    assert_eq!(repo.stash_list().unwrap().len(), 1);
    drop(repo);

    assert_unchanged(&before, &snapshot(fx.path()).unwrap());
}
