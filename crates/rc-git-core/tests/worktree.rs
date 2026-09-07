//! The working tree as a read-only view with a status overlay.
#![expect(
    clippy::disallowed_methods,
    reason = "tests arrange the fixture's working tree; the library never mutates"
)]

use rc_git_core::{BlobContent, DEFAULT_BLOB_LIMIT, EntryKind, Error, Repository, WorktreeStatus};
use rc_git_testkit::Fixture;

fn statuses(listing: &rc_git_core::WorktreeListing) -> Vec<(String, WorktreeStatus)> {
    listing
        .entries
        .iter()
        .map(|entry| (entry.name.to_string(), entry.status))
        .collect()
}

#[test]
fn a_clean_checkout_is_all_unchanged() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let listing = repo.worktree_entries(tree, "").unwrap();
    assert!(listing.missing.is_empty());
    assert_eq!(
        statuses(&listing),
        [
            ("docs".to_owned(), WorktreeStatus::Unchanged),
            ("src".to_owned(), WorktreeStatus::Unchanged),
            ("alpha.txt".to_owned(), WorktreeStatus::Unchanged),
            ("bin.dat".to_owned(), WorktreeStatus::Unchanged),
            ("link".to_owned(), WorktreeStatus::Unchanged),
            ("README.md".to_owned(), WorktreeStatus::Unchanged),
            ("Zeta.txt".to_owned(), WorktreeStatus::Unchanged),
        ]
    );
    let readme = listing
        .entries
        .iter()
        .find(|entry| entry.name == "README.md")
        .unwrap();
    assert_eq!(readme.oid.unwrap().to_hex(), fx.rev_parse("HEAD:README.md"));
    assert!(readme.size > 0);

    let sub = repo.worktree_entries(tree, "src").unwrap();
    assert_eq!(
        statuses(&sub),
        [
            ("lib.rs".to_owned(), WorktreeStatus::Unchanged),
            ("main.rs".to_owned(), WorktreeStatus::Unchanged),
        ]
    );
}

#[test]
fn edits_deletions_and_new_files_are_flagged() {
    let fx = Fixture::standard();
    // Same size, different content: only the hash can tell.
    fx.write("alpha.txt", b"omega\n");
    // Different size.
    fx.write("README.md", b"changed\n");
    fx.write("untracked.txt", b"new\n");
    fx.write(".gitignore", b"*.tmp\n");
    fx.write("scratch.tmp", b"ignored\n");
    std::fs::remove_file(fx.path().join("Zeta.txt")).unwrap();

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let listing = repo.worktree_entries(tree, "").unwrap();
    let by_name: std::collections::HashMap<String, WorktreeStatus> =
        statuses(&listing).into_iter().collect();
    assert_eq!(by_name["alpha.txt"], WorktreeStatus::Modified);
    assert_eq!(by_name["README.md"], WorktreeStatus::Modified);
    assert_eq!(by_name["untracked.txt"], WorktreeStatus::Untracked);
    assert_eq!(by_name[".gitignore"], WorktreeStatus::Untracked);
    assert_eq!(by_name["scratch.tmp"], WorktreeStatus::Ignored);
    assert_eq!(by_name["bin.dat"], WorktreeStatus::Unchanged);
    assert!(!by_name.contains_key("Zeta.txt"));
    assert_eq!(listing.missing.len(), 1);
    assert_eq!(listing.missing[0].name, "Zeta.txt");
}

#[test]
fn a_changed_symlink_is_modified() {
    let fx = Fixture::standard();
    std::fs::remove_file(fx.path().join("link")).unwrap();
    fx.symlink("alpha.txt", "link");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let listing = repo.worktree_entries(tree, "").unwrap();
    let link = listing
        .entries
        .iter()
        .find(|entry| entry.name == "link")
        .unwrap();
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.status, WorktreeStatus::Modified);
}

#[test]
fn a_directory_not_in_the_tree_is_untracked() {
    let fx = Fixture::standard();
    fx.write("newdir/file.txt", b"x\n");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let listing = repo.worktree_entries(tree, "").unwrap();
    let dir = listing
        .entries
        .iter()
        .find(|entry| entry.name == "newdir")
        .unwrap();
    assert_eq!(
        (dir.kind, dir.status),
        (EntryKind::Tree, WorktreeStatus::Untracked)
    );
    assert!(dir.oid.is_none());

    let inside = repo.worktree_entries(tree, "newdir").unwrap();
    assert_eq!(
        statuses(&inside),
        [("file.txt".to_owned(), WorktreeStatus::Untracked)]
    );
    assert!(inside.missing.is_empty());
}

#[test]
fn worktree_file_reads_text_binary_symlink_and_limits() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();

    let BlobContent::Text { data, line_starts } =
        repo.worktree_file("README.md", DEFAULT_BLOB_LIMIT).unwrap()
    else {
        panic!("expected text");
    };
    assert_eq!(data, rc_git_testkit::README);
    assert_eq!(line_starts, [0, 10, 11]);

    let BlobContent::Binary { size } = repo.worktree_file("bin.dat", DEFAULT_BLOB_LIMIT).unwrap()
    else {
        panic!("expected binary");
    };
    assert_eq!(size, rc_git_testkit::BIN_DAT.len() as u64);

    let BlobContent::TooLarge { size, limit } = repo.worktree_file("README.md", 3).unwrap() else {
        panic!("expected too large");
    };
    assert_eq!((size, limit), (17, 3));

    let BlobContent::Text { data, .. } = repo.worktree_file("link", DEFAULT_BLOB_LIMIT).unwrap()
    else {
        panic!("expected symlink target as text");
    };
    assert_eq!(data, b"README.md");

    let err = repo
        .worktree_file("nope.txt", DEFAULT_BLOB_LIMIT)
        .unwrap_err();
    assert!(matches!(err, Error::Worktree { .. }), "{err}");
    assert!(err.to_string().contains("nope.txt"));

    let err = repo
        .worktree_file("../outside", DEFAULT_BLOB_LIMIT)
        .unwrap_err();
    assert!(err.to_string().contains("may not leave"), "{err}");

    let err = repo.worktree_file("src", DEFAULT_BLOB_LIMIT).unwrap_err();
    assert!(err.to_string().contains("is a directory"), "{err}");
}

#[test]
fn a_bare_repository_has_no_working_tree() {
    let fx = Fixture::standard();
    let bare = Fixture::empty_dir();
    bare.git(&[
        "clone",
        "--bare",
        "-q",
        fx.path().to_str().unwrap(),
        bare.path().join("bare.git").to_str().unwrap(),
    ]);
    let repo = Repository::open(bare.path().join("bare.git")).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let err = repo.worktree_entries(tree, "").unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
    let err = repo
        .worktree_file("README.md", DEFAULT_BLOB_LIMIT)
        .unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
}

#[test]
fn a_missing_directory_is_an_error_but_a_tree_only_directory_lists_as_missing() {
    let fx = Fixture::standard();
    std::fs::remove_dir_all(fx.path().join("docs")).unwrap();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let listing = repo.worktree_entries(tree, "").unwrap();
    assert_eq!(listing.missing.len(), 1);
    assert_eq!(listing.missing[0].name, "docs");

    let err = repo.worktree_entries(tree, "docs").unwrap_err();
    assert!(matches!(err, Error::Worktree { .. }), "{err}");
}

#[test]
fn worktree_status_lists_every_change_flat_and_sorted() {
    let fx = Fixture::standard();
    // Same size, different content: only the hash can tell.
    fx.write("alpha.txt", b"omega\n");
    fx.write("untracked.txt", b"new\n");
    fx.write("newdir/inner/file.txt", b"x\n");
    std::fs::remove_file(fx.path().join("src/lib.rs")).unwrap();
    std::fs::remove_dir_all(fx.path().join("docs")).unwrap();

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(tree, 10_000).unwrap();
    assert!(!report.truncated);
    assert!(report.scanned > 0);

    let rows: Vec<(String, WorktreeStatus)> = report
        .entries
        .iter()
        .map(|entry| (entry.path.to_string(), entry.status))
        .collect();
    assert_eq!(
        rows,
        [
            ("alpha.txt".to_owned(), WorktreeStatus::Modified),
            ("docs/guide.md".to_owned(), WorktreeStatus::Deleted),
            (
                "newdir/inner/file.txt".to_owned(),
                WorktreeStatus::Untracked
            ),
            ("src/lib.rs".to_owned(), WorktreeStatus::Deleted),
            ("untracked.txt".to_owned(), WorktreeStatus::Untracked),
        ]
    );

    let modified = &report.entries[0];
    assert_eq!(
        modified.oid.unwrap().to_hex(),
        fx.rev_parse("HEAD:alpha.txt")
    );
    assert_eq!(modified.size, 6);
    let deleted = &report.entries[1];
    assert_eq!(
        deleted.oid.unwrap().to_hex(),
        fx.rev_parse("HEAD:docs/guide.md")
    );
    assert_eq!((deleted.kind, deleted.size), (EntryKind::Blob, 0));
    let untracked = &report.entries[2];
    assert!(untracked.oid.is_none());
}

#[test]
fn worktree_status_flags_exec_bit_and_symlink_changes() {
    let fx = Fixture::standard();
    let alpha = fx.path().join("alpha.txt");
    let mut perms = std::fs::metadata(&alpha).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&alpha, perms).unwrap();
    std::fs::remove_file(fx.path().join("link")).unwrap();
    fx.symlink("Zeta.txt", "link");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(tree, 10_000).unwrap();
    let rows: Vec<(String, EntryKind, WorktreeStatus)> = report
        .entries
        .iter()
        .map(|entry| (entry.path.to_string(), entry.kind, entry.status))
        .collect();
    assert_eq!(
        rows,
        [
            (
                "alpha.txt".to_owned(),
                EntryKind::ExecutableBlob,
                WorktreeStatus::Modified
            ),
            (
                "link".to_owned(),
                EntryKind::Symlink,
                WorktreeStatus::Modified
            ),
        ]
    );
}

#[test]
fn worktree_status_prunes_ignored_directories_unopened() {
    let fx = Fixture::standard();
    fx.write(".gitignore", b"junk/\n");
    for i in 0..50 {
        fx.write(&format!("junk/file{i}.tmp"), b"x\n");
    }

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(tree, 10_000).unwrap();
    let rows: Vec<String> = report
        .entries
        .iter()
        .map(|entry| entry.path.to_string())
        .collect();
    assert_eq!(rows, [".gitignore".to_owned()]);
    // The pruned directory contributes one visited entry, never its 50 files.
    assert!(report.scanned < 15, "scanned {}", report.scanned);
}

#[test]
fn worktree_status_truncates_at_the_scan_cap() {
    let fx = Fixture::standard();
    for i in 0..20 {
        fx.write(&format!("many/file{i}.txt"), b"x\n");
    }

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(tree, 5).unwrap();
    assert!(report.truncated);
    assert_eq!(report.scanned, 6);

    let full = repo.worktree_status(tree, 100_000).unwrap();
    assert!(!full.truncated);
    assert_eq!(full.entries.len(), 20);
}

#[test]
fn worktree_status_reports_a_file_replacing_a_tracked_directory() {
    let fx = Fixture::standard();
    std::fs::remove_dir_all(fx.path().join("docs")).unwrap();
    fx.write("docs", b"now a file\n");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(tree, 10_000).unwrap();
    let rows: Vec<(String, WorktreeStatus)> = report
        .entries
        .iter()
        .map(|entry| (entry.path.to_string(), entry.status))
        .collect();
    assert_eq!(
        rows,
        [
            ("docs".to_owned(), WorktreeStatus::Modified),
            ("docs/guide.md".to_owned(), WorktreeStatus::Deleted),
        ]
    );
    assert!(report.entries[0].oid.is_none());
}

#[test]
fn worktree_status_on_a_bare_repository_is_an_error() {
    let fx = Fixture::standard();
    let bare = Fixture::empty_dir();
    fx.git(&[
        "clone",
        "--bare",
        "-q",
        fx.path().to_str().unwrap(),
        bare.path().join("bare.git").to_str().unwrap(),
    ]);
    let repo = Repository::open(bare.path().join("bare.git")).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let err = repo.worktree_status(tree, 10_000).unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
}
