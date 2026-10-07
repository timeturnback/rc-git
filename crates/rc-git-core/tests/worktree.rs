//! The working tree as a read-only view with a status overlay.

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
    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    assert_eq!(listing.missing.len(), 0);
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

    let sub = repo.worktree_entries(Some(tree), "src").unwrap();
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
    fx.remove("Zeta.txt");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let listing = repo.worktree_entries(Some(tree), "").unwrap();
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
    fx.remove("link");
    fx.symlink("alpha.txt", "link");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    let link = listing
        .entries
        .iter()
        .find(|entry| entry.name == "link")
        .unwrap();
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.status, WorktreeStatus::Modified);
}

/// Commits a symlink entry at `path` that points at `oid`, then puts a real symlink on disk.
fn commit_symlink_entry(fx: &Fixture, oid: &str, path: &str) {
    fx.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("120000,{oid},{path}"),
    ]);
    fx.git(&["commit", "-q", "-m", "odd symlink"]);
    fx.symlink("x", path);
}

fn status_of(fx: &Fixture, path: &str) -> Result<WorktreeStatus, Error> {
    let repo = Repository::open(fx.path())?;
    let tree = repo.resolve_tree("HEAD")?;
    let report = repo.worktree_status(Some(tree), 10_000)?;
    Ok(report
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .map_or(WorktreeStatus::Unchanged, |entry| entry.status))
}

#[test]
fn a_symlink_entry_holding_a_huge_blob_is_modified_without_decoding() {
    let fx = Fixture::standard();
    fx.write("big.bin", &vec![b'y'; 8192]);
    let oid = fx.git_output(&["hash-object", "-w", "big.bin"]);
    fx.remove("big.bin");
    commit_symlink_entry(&fx, oid.trim(), "biglink");
    assert_eq!(status_of(&fx, "biglink").unwrap(), WorktreeStatus::Modified);
}

#[test]
fn a_symlink_entry_naming_a_tree_is_modified_not_an_error() {
    let fx = Fixture::standard();
    let tree = fx.rev_parse("HEAD^{tree}");
    commit_symlink_entry(&fx, &tree, "treelink");
    assert_eq!(
        status_of(&fx, "treelink").unwrap(),
        WorktreeStatus::Modified
    );
}

#[test]
fn a_directory_not_in_the_tree_is_untracked() {
    let fx = Fixture::standard();
    fx.write("newdir/file.txt", b"x\n");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let listing = repo.worktree_entries(Some(tree), "").unwrap();
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

    let inside = repo.worktree_entries(Some(tree), "newdir").unwrap();
    assert_eq!(
        statuses(&inside),
        [("file.txt".to_owned(), WorktreeStatus::Untracked)]
    );
    assert_eq!(inside.missing.len(), 0);
}

#[test]
fn worktree_file_reads_text_binary_symlink_and_limits() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();

    let BlobContent::Text {
        data,
        line_starts,
        valid_utf8,
    } = repo.worktree_file("README.md", DEFAULT_BLOB_LIMIT).unwrap()
    else {
        panic!("expected text");
    };
    assert_eq!(data, rc_git_testkit::README);
    assert_eq!(line_starts, [0, 10, 11]);
    assert!(valid_utf8);

    let BlobContent::Binary { size } = repo.worktree_file("bin.dat", DEFAULT_BLOB_LIMIT).unwrap()
    else {
        panic!("expected binary");
    };
    assert_eq!(size, rc_git_testkit::BIN_DAT.len() as u64);

    let BlobContent::TooLarge { size, limit } = repo.worktree_file("README.md", 3).unwrap() else {
        panic!("expected too large");
    };
    assert_eq!((size, limit), (rc_git_testkit::README.len() as u64, 3));

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
    let (_bare, bare_path) = fx.bare_copy();
    let repo = Repository::open(&bare_path).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let err = repo.worktree_entries(Some(tree), "").unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
    let err = repo
        .worktree_file("README.md", DEFAULT_BLOB_LIMIT)
        .unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
    let err = repo.worktree_status(Some(tree), 10_000).unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
    let err = repo.worktree_files(Some(tree), 10_000).unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
}

#[test]
fn every_spelling_of_a_directory_lists_the_same_entries() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let root = repo.worktree_entries(Some(tree), "").unwrap();
    for spelling in ["/", ".", "./", "//"] {
        assert_eq!(
            repo.worktree_entries(Some(tree), spelling).unwrap(),
            root,
            "root spelled {spelling:?}"
        );
    }
    let src = repo.worktree_entries(Some(tree), "src").unwrap();
    for spelling in ["./src", "/src/", "src//", "./src/."] {
        assert_eq!(
            repo.worktree_entries(Some(tree), spelling).unwrap(),
            src,
            "src spelled {spelling:?}"
        );
    }

    let err = repo.worktree_entries(Some(tree), "src/..").unwrap_err();
    assert!(matches!(err, Error::Worktree { .. }), "{err}");
    assert!(err.to_string().contains("'..'"), "{err}");
}

#[test]
fn worktree_reads_refuse_a_path_through_a_symlinked_directory() {
    let fx = Fixture::standard();
    let outside = Fixture::empty_dir();
    outside.write("secret.txt", b"secret\n");
    outside.write("nested/deeper.txt", b"deeper\n");
    fx.symlink(outside.path().to_str().unwrap(), "escape");
    fx.commit("Link outside", "2024-01-04T00:00:00+0000");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    for path in [
        "escape/secret.txt",
        "./escape/secret.txt",
        "escape/nested/deeper.txt",
    ] {
        let err = repo.worktree_file(path, DEFAULT_BLOB_LIMIT).unwrap_err();
        assert!(matches!(err, Error::Worktree { .. }), "{path}: {err}");
        assert!(
            err.to_string().contains("symbolic link at 'escape'"),
            "{path}: {err}"
        );
    }
    for dir in ["escape", "escape/", "./escape", "escape/nested"] {
        let err = repo.worktree_entries(Some(tree), dir).unwrap_err();
        assert!(matches!(err, Error::Worktree { .. }), "{dir}: {err}");
        assert!(
            err.to_string().contains("symbolic link at 'escape'"),
            "{dir}: {err}"
        );
    }

    let BlobContent::Text { data, .. } = repo.worktree_file("escape", DEFAULT_BLOB_LIMIT).unwrap()
    else {
        panic!("the link itself still reads as its target");
    };
    assert_eq!(data, outside.path().to_str().unwrap().as_bytes());
}

#[test]
fn a_missing_directory_is_an_error_but_a_tree_only_directory_lists_as_missing() {
    let fx = Fixture::standard();
    fx.remove("docs");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    assert_eq!(listing.missing.len(), 1);
    assert_eq!(listing.missing[0].name, "docs");

    let err = repo.worktree_entries(Some(tree), "docs").unwrap_err();
    assert!(matches!(err, Error::Worktree { .. }), "{err}");
}

#[test]
fn worktree_status_lists_every_change_flat_and_sorted() {
    let fx = Fixture::standard();
    // Same size, different content: only the hash can tell.
    fx.write("alpha.txt", b"omega\n");
    fx.write("untracked.txt", b"new\n");
    fx.write("newdir/inner/file.txt", b"x\n");
    fx.remove("src/lib.rs");
    fx.remove("docs");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
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
fn an_unborn_branch_scans_with_no_tree_and_lists_every_file_as_untracked() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("a.txt", b"hi\n");
    fx.write("src/b.txt", b"b\n");
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(repo.head().unwrap().target, None);

    let report = repo.worktree_status(None, 10_000).unwrap();
    let rows: Vec<(String, WorktreeStatus, bool)> = report
        .entries
        .iter()
        .map(|entry| (entry.path.to_string(), entry.status, entry.oid.is_some()))
        .collect();
    assert_eq!(
        rows,
        [
            ("a.txt".to_owned(), WorktreeStatus::Untracked, false),
            ("src/b.txt".to_owned(), WorktreeStatus::Untracked, false),
        ]
    );

    let listing = repo.worktree_entries(None, "").unwrap();
    assert_eq!(
        statuses(&listing),
        [
            ("src".to_owned(), WorktreeStatus::Untracked),
            ("a.txt".to_owned(), WorktreeStatus::Untracked),
        ]
    );
    assert_eq!(listing.missing.len(), 0);
    let paths: Vec<String> = repo
        .worktree_files(None, 10_000)
        .unwrap()
        .files
        .iter()
        .map(|file| file.path.to_string())
        .collect();
    assert_eq!(paths, ["a.txt", "src/b.txt"]);
}

#[test]
fn worktree_status_flags_exec_bit_and_symlink_changes() {
    let fx = Fixture::standard();
    fx.set_mode("alpha.txt", 0o755);
    fx.remove("link");
    fx.symlink("Zeta.txt", "link");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
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
fn only_the_owner_exec_bit_counts_as_a_mode_change() {
    let fx = Fixture::standard();
    fx.set_mode("alpha.txt", 0o645);

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    assert_eq!(
        repo.worktree_status(Some(tree), 10_000).unwrap().entries,
        []
    );
    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    let alpha = listing
        .entries
        .iter()
        .find(|entry| entry.name == "alpha.txt")
        .unwrap();
    assert_eq!(
        (alpha.kind, alpha.status),
        (EntryKind::Blob, WorktreeStatus::Unchanged)
    );
}

#[test]
fn with_file_mode_off_the_disk_exec_bit_is_ignored() {
    let fx = Fixture::standard();
    fx.git(&["config", "core.fileMode", "false"]);
    fx.set_mode("alpha.txt", 0o755);
    fx.write("fresh.sh", b"echo\n");
    fx.set_mode("fresh.sh", 0o755);
    assert_eq!(fx.git_output(&["status", "--porcelain"]), "?? fresh.sh");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let rows: Vec<(String, EntryKind, WorktreeStatus)> = repo
        .worktree_status(Some(tree), 10_000)
        .unwrap()
        .entries
        .iter()
        .map(|entry| (entry.path.to_string(), entry.kind, entry.status))
        .collect();
    assert_eq!(
        rows,
        [(
            "fresh.sh".to_owned(),
            EntryKind::Blob,
            WorktreeStatus::Untracked
        )]
    );
    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    let alpha = listing
        .entries
        .iter()
        .find(|entry| entry.name == "alpha.txt")
        .unwrap();
    assert_eq!(
        (alpha.kind, alpha.status),
        (EntryKind::Blob, WorktreeStatus::Unchanged)
    );
    let files = repo.worktree_files(Some(tree), 10_000).unwrap().files;
    let kinds: Vec<EntryKind> = files
        .iter()
        .filter(|file| file.path == "alpha.txt" || file.path == "fresh.sh")
        .map(|file| file.kind)
        .collect();
    assert_eq!(kinds, [EntryKind::Blob, EntryKind::Blob]);
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
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
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
    let report = repo.worktree_status(Some(tree), 5).unwrap();
    assert!(report.truncated);
    assert_eq!(report.scanned, 5);

    let full = repo.worktree_status(Some(tree), 100_000).unwrap();
    assert!(!full.truncated);
    assert_eq!(full.entries.len(), 20);
}

#[test]
fn worktree_status_reports_a_file_replacing_a_tracked_directory() {
    let fx = Fixture::standard();
    fx.remove("docs");
    fx.write("docs", b"now a file\n");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
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
fn worktree_status_reports_a_directory_replacing_a_tracked_file() {
    let fx = Fixture::standard();
    fx.remove("alpha.txt");
    fx.write("alpha.txt/inner.txt", b"in\n");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    let rows: Vec<(String, WorktreeStatus)> = report
        .entries
        .iter()
        .map(|entry| (entry.path.to_string(), entry.status))
        .collect();
    assert_eq!(
        rows,
        [
            ("alpha.txt".to_owned(), WorktreeStatus::Deleted),
            ("alpha.txt/inner.txt".to_owned(), WorktreeStatus::Untracked),
        ]
    );
    assert_eq!(
        report.entries[0].oid.unwrap().to_hex(),
        fx.rev_parse("HEAD:alpha.txt")
    );
    assert_eq!(report.entries[0].kind, EntryKind::Blob);
}

/// The index is consulted as a stat cache to skip hashing unchanged files. It may only ever
/// save that hash, never answer in place of one, so every way an entry can stop describing
/// what is on disk has to fall back to reading the file.
#[test]
fn the_index_stat_cache_never_vouches_for_a_changed_file() {
    let fx = Fixture::standard();
    // A second must pass before the edits, so that the index written after them carries a
    // later timestamp than the stat of every file left alone. Without that gap every entry
    // is racy, the cache declines them all, and the test cannot tell it from a broken one.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    // Same size as the blob, so only content can decide.
    fx.write("alpha.txt", b"omega\n");
    // Rewritten with what it already held: the stat moves, the content does not.
    let readme = std::fs::read(fx.path().join("README.md")).unwrap();
    fx.write("README.md", &readme);
    // Staged with a different size, so the size check settles it before the cache is asked.
    fx.write("Zeta.txt", b"omega\n");
    // This `git add` also rewrites the index after the sleep, which is what lets the cache
    // answer for the untouched files.
    fx.git(&["add", "Zeta.txt"]);

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    let rows: std::collections::HashMap<String, WorktreeStatus> = report
        .entries
        .iter()
        .map(|entry| (entry.path.to_string(), entry.status))
        .collect();
    assert_eq!(rows.get("alpha.txt"), Some(&WorktreeStatus::Modified));
    assert_eq!(rows.get("Zeta.txt"), Some(&WorktreeStatus::Modified));
    assert_eq!(
        rows.get("README.md"),
        None,
        "a moved mtime alone is no edit"
    );
    assert_eq!(rows.get("src/main.rs"), None, "an untouched file is clean");
}

/// A clean file the index vouches for is never opened: made unreadable after the index was
/// rewritten, it still reads as clean, where a cache that never vouches fails to hash it.
#[test]
fn the_stat_cache_answers_without_reading_a_clean_file() {
    let fx = Fixture::standard();
    fx.git(&["config", "core.trustctime", "false"]);
    // A second must pass so the index the `git add` writes is later than every untouched file.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fx.write("Zeta.txt", b"omega\n");
    fx.git(&["add", "Zeta.txt"]);
    // After the `git add`: git re-reads racily clean entries whenever it writes the index.
    fx.set_mode("README.md", 0o000);

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000);
    fx.set_mode("README.md", 0o644);

    let report = report.unwrap();
    assert!(
        report.unreadable.is_empty(),
        "the cache should have vouched for README.md: {:?}",
        report.unreadable
    );
    assert!(
        report.entries.iter().all(|entry| entry.path != "README.md"),
        "{:?}",
        report.entries
    );
}

/// Overwrites a file in place, keeping its inode, and puts its mtime back, so only the
/// content and the ctime differ from what the index recorded.
fn rewrite_keeping_mtime(fx: &Fixture, relative: &str, contents: &[u8]) -> std::io::Result<()> {
    let mtime = std::fs::metadata(fx.path().join(relative))?.modified()?;
    fx.write(relative, contents);
    set_mtime(fx, relative, mtime)
}

/// Sets a fixture file's mtime, so a test can match or outrun the stat the index recorded.
#[expect(
    clippy::disallowed_methods,
    reason = "the test edits its own fixture file behind the index's back"
)]
fn set_mtime(fx: &Fixture, relative: &str, mtime: std::time::SystemTime) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(fx.path().join(relative))?
        .set_modified(mtime)
}

/// An entry flagged with `flag` keeps a stat that git no longer checks, so it is no evidence:
/// a same-size edit with the mtime put back must still be read and reported.
fn status_of_flagged_same_stat_edit(
    flag: &str,
) -> Result<Vec<(String, WorktreeStatus)>, Box<dyn std::error::Error>> {
    let fx = Fixture::standard();
    fx.git(&["config", "core.trustctime", "false"]);
    // Without the gap the entry is racy and that check declines before the flag is looked at.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fx.git(&["update-index", flag, "alpha.txt"]);
    rewrite_keeping_mtime(&fx, "alpha.txt", b"omega\n")?;

    let repo = Repository::open(fx.path())?;
    let tree = repo.resolve_tree("HEAD")?;
    let report = repo.worktree_status(Some(tree), 10_000)?;
    Ok(status_paths(&report))
}

#[test]
fn an_assume_unchanged_entry_is_read_anyway() {
    assert_eq!(
        status_of_flagged_same_stat_edit("--assume-unchanged").unwrap(),
        [("alpha.txt".to_owned(), WorktreeStatus::Modified)]
    );
}

#[test]
fn a_skip_worktree_entry_is_read_anyway() {
    assert_eq!(
        status_of_flagged_same_stat_edit("--skip-worktree").unwrap(),
        [("alpha.txt".to_owned(), WorktreeStatus::Modified)]
    );
}

/// A staged edit of the blob's size leaves a fresh, non-racy stat in the index, but for a
/// different blob than the tree's, so the cache must not vouch for it against the tree.
#[test]
fn a_staged_same_size_edit_is_modified_against_the_tree() {
    let fx = Fixture::standard();
    fx.write("Zeta.txt", b"ZETA\n");
    // Between the write and the `git add`, or the entry is racy and the id check never runs.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fx.git(&["add", "Zeta.txt"]);

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    assert_eq!(
        status_paths(&report),
        [("Zeta.txt".to_owned(), WorktreeStatus::Modified)]
    );
}

/// A file edited no earlier than the index was written is indistinguishable from one edited
/// just after it, so the entry proves nothing and the file must be read. The future mtime makes
/// the entry racy by construction, with every other compared stat field matching.
#[test]
fn a_racy_index_entry_still_reports_the_edit() {
    let fx = Fixture::standard();
    fx.git(&["config", "core.trustctime", "false"]);
    let tomorrow = std::time::SystemTime::now() + std::time::Duration::from_hours(24);
    set_mtime(&fx, "alpha.txt", tomorrow).unwrap();
    // The index now records an mtime later than its own timestamp, so the entry stays racy.
    fx.git(&["update-index", "--refresh"]);
    fx.write("alpha.txt", b"omega\n");
    set_mtime(&fx, "alpha.txt", tomorrow).unwrap();

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    assert_eq!(report.entries.len(), 1, "{:?}", report.entries);
    assert_eq!(report.entries[0].path, "alpha.txt");
    assert_eq!(report.entries[0].status, WorktreeStatus::Modified);
}

/// Under an eol conversion the checked-out file never has the blob's size, so only the index,
/// which recorded the size git wrote, can say it is clean. A size check ahead of the stat cache
/// reported every such file as modified.
#[test]
fn a_converted_file_the_index_vouches_for_is_clean() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write(".gitattributes", b"* text eol=crlf\n");
    fx.write("a.txt", b"a\nb\n");
    fx.commit("one", "2024-01-01T00:00:00Z");
    fx.remove("a.txt");
    fx.git(&["checkout", "-q", "--", "a.txt"]);
    assert_eq!(
        std::fs::read(fx.path().join("a.txt")).unwrap(),
        b"a\r\nb\r\n"
    );
    // A second must pass so the index `git status` rewrites is later than the file's stat;
    // otherwise the entry is racy and the cache rightly declines it.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(fx.git_output(&["status", "--porcelain"]), "");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    assert!(report.entries.is_empty(), "{:?}", report.entries);
}

fn file_paths(list: &rc_git_core::WorktreeFileList) -> Vec<String> {
    list.files
        .iter()
        .map(|file| file.path.to_string())
        .collect()
}

#[test]
fn worktree_files_lists_tracked_untracked_and_ignored_files_sorted() {
    let fx = Fixture::standard();
    fx.write(".gitignore", b".env\njunk/\n");
    fx.write(".env", b"SECRET=1\n");
    fx.write(".DS_Store", b"\0");
    fx.write("notes/todo.txt", b"x\n");
    for i in 0..50 {
        fx.write(&format!("junk/file{i}.tmp"), b"x\n");
    }

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let list = repo.worktree_files(Some(tree), 10_000).unwrap();
    assert!(!list.truncated);
    assert_eq!(
        file_paths(&list),
        [
            ".DS_Store",
            ".env",
            ".gitignore",
            "README.md",
            "Zeta.txt",
            "alpha.txt",
            "bin.dat",
            "docs/guide.md",
            "link",
            "notes/todo.txt",
            "src/lib.rs",
            "src/main.rs",
        ]
    );
    let link = list.files.iter().find(|file| file.path == "link").unwrap();
    assert_eq!(link.kind, EntryKind::Symlink);
    // The ignored directory costs one visited entry, never its 50 files.
    assert!(list.scanned < 25, "scanned {}", list.scanned);
}

#[test]
fn worktree_files_enters_an_ignored_directory_that_holds_tracked_files() {
    let fx = Fixture::standard();
    fx.write(".gitignore", b"vendor/\n");
    fx.write("vendor/kept.txt", b"kept\n");
    fx.git(&["add", "-f", ".gitignore", "vendor/kept.txt"]);
    fx.commit("Vendor one file", "2024-01-04T00:00:00+0000");
    fx.write("vendor/extra.txt", b"extra\n");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let list = repo.worktree_files(Some(tree), 10_000).unwrap();
    let vendored: Vec<String> = file_paths(&list)
        .into_iter()
        .filter(|path| path.starts_with("vendor/"))
        .collect();
    assert_eq!(vendored, ["vendor/extra.txt", "vendor/kept.txt"]);
}

#[test]
fn worktree_status_stops_inside_a_deleted_tree_at_the_cap() {
    let fx = Fixture::standard();
    for i in 0..50 {
        // Distinct contents give each subtree its own object.
        fx.write(&format!("d/p{i:02}/f"), format!("{i}\n").as_bytes());
    }
    fx.commit("Add d", "2024-01-05T00:00:00+0000");
    fx.remove("d");
    let full = {
        let repo = Repository::open(fx.path()).unwrap();
        repo.worktree_status(Some(repo.resolve_tree("HEAD").unwrap()), 100_000)
            .unwrap()
    };
    assert!(!full.truncated);
    // Corrupt the last deleted subtree: a walk that keeps decoding past the cap fails on it.
    let last = fx.rev_parse("HEAD:d/p49");
    fx.remove(&format!(".git/objects/{}/{}", &last[..2], &last[2..]));

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let cap = full.scanned - 25;
    let report = repo.worktree_status(Some(tree), cap).unwrap();
    assert!(report.truncated);
    assert_eq!(report.scanned, cap);
}

#[test]
fn worktree_files_truncates_at_the_scan_cap() {
    let fx = Fixture::standard();
    for i in 0..20 {
        fx.write(&format!("many/file{i}.txt"), b"x\n");
    }

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let list = repo.worktree_files(Some(tree), 5).unwrap();
    assert!(list.truncated);
    assert_eq!(list.scanned, 5);

    let full = repo.worktree_files(Some(tree), 100_000).unwrap();
    assert!(!full.truncated);
    assert_eq!(full.files.len(), 28);
}

#[test]
fn worktree_files_reports_an_unreadable_directory() {
    let fx = Fixture::standard();
    fx.set_mode("src", 0o000);
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let list = repo.worktree_files(Some(tree), 10_000);
    fx.set_mode("src", 0o755);

    let list = list.unwrap();
    assert!(!list.files.iter().any(|file| file.path.starts_with(b"src/")));
    assert!(list.files.iter().any(|file| file.path == "README.md"));
    assert_eq!(list.unreadable.len(), 1, "{:?}", list.unreadable);
    assert_eq!(list.unreadable[0].path, "src");
    assert!(
        list.unreadable[0].error.starts_with("read directory: "),
        "{}",
        list.unreadable[0].error
    );
}

#[test]
fn worktree_files_fails_on_an_unreadable_root() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    fx.set_mode("", 0o000);
    let err = repo.worktree_files(None, 10_000);
    fx.set_mode("", 0o755);

    let err = err.unwrap_err().to_string();
    assert!(err.contains("list working tree files: ''"), "{err}");
}

#[test]
fn an_unreadable_untracked_directory_is_skipped_and_the_rest_listed() {
    let fx = Fixture::standard();
    fx.write("locked/secret.txt", b"s\n");
    fx.write("new.txt", b"n\n");
    fx.set_mode("locked", 0o000);
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000);
    let list = repo.worktree_files(Some(tree), 10_000);
    fx.set_mode("locked", 0o755);

    let report = report.unwrap();
    let changed: Vec<String> = report.entries.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(changed, ["new.txt"]);
    assert_eq!(report.unreadable.len(), 1, "{:?}", report.unreadable);
    assert_eq!(report.unreadable[0].path, "locked");
    assert!(
        report.unreadable[0].error.starts_with("read directory: "),
        "{}",
        report.unreadable[0].error
    );

    let list = list.unwrap();
    assert!(list.files.iter().any(|file| file.path == "new.txt"));
    assert!(
        !list
            .files
            .iter()
            .any(|file| file.path.starts_with(b"locked"))
    );
    assert_eq!(list.unreadable.len(), 1, "{:?}", list.unreadable);
    assert_eq!(list.unreadable[0].path, "locked");
}

#[test]
fn an_unreadable_tracked_directory_is_not_reported_deleted() {
    let fx = Fixture::standard();
    fx.set_mode("src", 0o000);
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000);
    fx.set_mode("src", 0o755);

    let report = report.unwrap();
    assert_eq!(report.entries, []);
    let skipped: Vec<String> = report
        .unreadable
        .iter()
        .map(|e| e.path.to_string())
        .collect();
    assert_eq!(skipped, ["src"]);
}

#[test]
fn worktree_status_reports_an_unreadable_root() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    // Removing the root takes `.git` with it; reading the tree first leaves it in the handle's
    // object cache, so the walk reaches the root listing instead of failing on the tree read.
    repo.tree_entries(tree).unwrap();
    fx.remove("");
    let err = repo.worktree_status(Some(tree), 10_000).unwrap_err();
    assert!(err.to_string().contains("scan working tree"), "{err}");
}

/// A same-size edit, so the walk must open the file to hash it, then made unreadable.
fn with_unreadable_main_rs() -> Fixture {
    let fx = Fixture::standard();
    fx.write("src/main.rs", b"fn main() {\n    println!(\"HI\");\n}\n");
    fx.set_mode("src/main.rs", 0o000);
    fx
}

#[test]
fn worktree_status_names_an_unreadable_file() {
    let fx = with_unreadable_main_rs();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000);
    fx.set_mode("src/main.rs", 0o644);

    let report = report.unwrap();
    assert_eq!(report.entries, []);
    assert_eq!(report.unreadable.len(), 1, "{:?}", report.unreadable);
    assert_eq!(report.unreadable[0].path, "src/main.rs");
    assert!(
        report.unreadable[0]
            .error
            .starts_with("open working tree file: "),
        "{}",
        report.unreadable[0].error
    );
}

#[test]
fn a_listing_error_names_the_unreadable_file() {
    let fx = with_unreadable_main_rs();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let err = repo.worktree_entries(Some(tree), "src");
    fx.set_mode("src/main.rs", 0o644);

    let err = err.unwrap_err().to_string();
    assert!(err.contains("open working tree file"), "{err}");
    assert!(err.contains("'src/main.rs'"), "{err}");
}

/// The standard fixture with a checked-out submodule committed at `vendor/sub`, plus the
/// repository the submodule was cloned from, which must outlive the test.
fn with_submodule() -> (Fixture, Fixture) {
    let sub = Fixture::empty_dir();
    sub.git(&["init", "-q", "-b", "main"]);
    sub.write("x", b"x\n");
    sub.commit("sub", "2024-01-04T00:00:00+0000");
    let top = Fixture::standard();
    let sub_path = sub.path().to_string_lossy();
    top.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        &sub_path,
        "vendor/sub",
    ]);
    top.commit("add submodule", "2024-01-05T00:00:00+0000");
    (top, sub)
}

#[test]
fn a_checked_out_submodule_is_unchanged() {
    let (fx, _sub) = with_submodule();
    assert_eq!(fx.git_output(&["status", "--porcelain"]), "");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    assert_eq!(report.entries, []);

    let listing = repo.worktree_entries(Some(tree), "vendor").unwrap();
    assert_eq!(
        statuses(&listing),
        [("sub".to_owned(), WorktreeStatus::Unchanged)]
    );
    assert_eq!(listing.missing, []);
}

#[test]
fn listing_inside_a_submodule_reads_its_files_as_another_repository() {
    let (fx, _sub) = with_submodule();
    fx.write("vendor/sub/inner/q", b"q\n");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let at = repo.worktree_entries(Some(tree), "vendor/sub").unwrap();
    assert_eq!(
        statuses(&at),
        [
            ("inner".to_owned(), WorktreeStatus::Unchanged),
            ("x".to_owned(), WorktreeStatus::Unchanged),
        ]
    );
    assert!(at.entries.iter().all(|entry| entry.oid.is_none()));
    assert_eq!(at.missing, []);

    let below = repo
        .worktree_entries(Some(tree), "vendor/sub/inner")
        .unwrap();
    assert_eq!(
        statuses(&below),
        [("q".to_owned(), WorktreeStatus::Unchanged)]
    );
    assert_eq!(below.entries[0].oid, None);
}

fn status_paths(report: &rc_git_core::StatusReport) -> Vec<(String, WorktreeStatus)> {
    report
        .entries
        .iter()
        .map(|entry| (entry.path.to_string(), entry.status))
        .collect()
}

#[test]
fn an_untracked_fifo_is_never_listed_or_opened() {
    let fx = Fixture::standard();
    fx.mkfifo("pipe");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let err = repo.worktree_file("pipe", DEFAULT_BLOB_LIMIT).unwrap_err();
    assert!(err.to_string().contains("is not a regular file"), "{err}");
    let err = repo
        .diff_worktree_file(None, "pipe", DEFAULT_BLOB_LIMIT, 3)
        .unwrap_err();
    assert!(err.to_string().contains("is not a regular file"), "{err}");

    assert_eq!(
        status_paths(&repo.worktree_status(Some(tree), 10_000).unwrap()),
        []
    );
    let files = repo.worktree_files(Some(tree), 10_000).unwrap().files;
    assert!(files.iter().all(|file| file.path != "pipe"), "{files:?}");
    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    assert!(listing.entries.iter().all(|entry| entry.name != "pipe"));
}

#[test]
fn a_fifo_replacing_a_tracked_empty_file_is_modified_without_opening_it() {
    let fx = Fixture::standard();
    fx.write("empty", b"");
    fx.commit("Add empty", "2024-01-04T00:00:00+0000");
    fx.remove("empty");
    fx.mkfifo("empty");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    assert_eq!(
        status_paths(&repo.worktree_status(Some(tree), 10_000).unwrap()),
        [("empty".to_owned(), WorktreeStatus::Modified)]
    );
    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    let entry = listing
        .entries
        .iter()
        .find(|entry| entry.name == "empty")
        .unwrap();
    assert_eq!(
        (entry.kind, entry.status),
        (EntryKind::Blob, WorktreeStatus::Modified)
    );
    assert_eq!(entry.oid.unwrap().to_hex(), fx.rev_parse("HEAD:empty"));
    let files = repo.worktree_files(Some(tree), 10_000).unwrap().files;
    assert!(files.iter().all(|file| file.path != "empty"), "{files:?}");
}

#[test]
fn a_fifo_replacing_a_tracked_directory_leaves_its_files_deleted() {
    let fx = Fixture::standard();
    fx.remove("docs");
    fx.mkfifo("docs");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    assert_eq!(
        status_paths(&repo.worktree_status(Some(tree), 10_000).unwrap()),
        [("docs/guide.md".to_owned(), WorktreeStatus::Deleted)]
    );
    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    assert!(listing.entries.iter().all(|entry| entry.name != "docs"));
    assert!(listing.missing.iter().any(|entry| entry.name == "docs"));
}

/// The standard fixture with an untracked repository at `nested`, holding one commit, and a
/// stray `.git` directory below a tracked one.
fn with_nested_repository() -> Fixture {
    let fx = Fixture::standard();
    fx.write("nested/inner.txt", b"inner\n");
    fx.git(&["-C", "nested", "init", "-q", "-b", "main"]);
    fx.git(&["-C", "nested", "add", "inner.txt"]);
    fx.git(&["-C", "nested", "commit", "-q", "-m", "inner"]);
    fx.write("src/.git/config", b"[core]\n");
    fx
}

#[test]
fn worktree_status_never_enters_a_nested_repository_or_a_git_directory() {
    let fx = with_nested_repository();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    assert_eq!(status_paths(&report), []);
    // The root's entries, `nested` itself and `src`'s entries; nothing inside either `.git`.
    assert!(report.scanned < 15, "scanned {}", report.scanned);
}

#[test]
fn worktree_files_never_enters_a_nested_repository_or_a_git_directory() {
    let fx = with_nested_repository();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let paths = file_paths(&repo.worktree_files(Some(tree), 10_000).unwrap());
    assert!(
        paths
            .iter()
            .all(|path| !path.starts_with("nested/") && !path.contains(".git/")),
        "{paths:?}"
    );
    assert!(paths.contains(&"src/lib.rs".to_owned()), "{paths:?}");
}

#[test]
fn worktree_entries_never_lists_a_git_directory_at_any_depth() {
    let fx = with_nested_repository();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let root = repo.worktree_entries(Some(tree), "").unwrap();
    let nested = root
        .entries
        .iter()
        .find(|entry| entry.name == "nested")
        .unwrap();
    assert_eq!(
        (nested.kind, nested.status),
        (EntryKind::Tree, WorktreeStatus::Untracked)
    );
    assert_eq!(
        statuses(&repo.worktree_entries(Some(tree), "nested").unwrap()),
        [("inner.txt".to_owned(), WorktreeStatus::Untracked)]
    );
    let src = repo.worktree_entries(Some(tree), "src").unwrap();
    assert!(src.entries.iter().all(|entry| entry.name != ".git"));
}

#[test]
fn a_corrupt_index_keeps_worktree_ignore_rules_and_is_reported() {
    let fx = Fixture::standard();
    fx.write(".gitignore", b".env\nnode_modules/\n");
    fx.commit("ignore secrets", "2024-01-05T00:00:00Z");
    fx.write(".env", b"S=1\n");
    fx.write("node_modules/pkg/i.js", b"x\n");
    fx.write("new.txt", b"x\n");
    let index = std::fs::read(fx.git_dir().join("index")).unwrap();
    // Long enough to pass the length check, so gix itself rejects it.
    fx.write(".git/index", &index[..40]);
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    assert_eq!(
        status_paths(&report),
        [("new.txt".to_owned(), WorktreeStatus::Untracked)]
    );
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].starts_with("read index '"),
        "{:?}",
        report.warnings
    );

    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    let ignored: Vec<String> = listing
        .entries
        .iter()
        .filter(|entry| entry.status == WorktreeStatus::Ignored)
        .map(|entry| entry.name.to_string())
        .collect();
    assert_eq!(ignored, ["node_modules", ".env"]);
    assert_eq!(listing.warnings, report.warnings);

    let files = repo.worktree_files(Some(tree), 10_000).unwrap();
    assert!(
        !file_paths(&files)
            .iter()
            .any(|p| p.starts_with("node_modules/"))
    );
    assert_eq!(files.warnings, report.warnings);
}

#[test]
fn a_readable_index_reports_no_warnings() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    assert!(
        repo.worktree_status(Some(tree), 10_000)
            .unwrap()
            .warnings
            .is_empty()
    );
    assert!(
        repo.worktree_entries(Some(tree), "")
            .unwrap()
            .warnings
            .is_empty()
    );
    assert!(
        repo.worktree_files(Some(tree), 10_000)
            .unwrap()
            .warnings
            .is_empty()
    );
}

/// A name listed by its path in `files`.
fn has_file(files: &rc_git_core::WorktreeFileList, path: &str) -> bool {
    files.files.iter().any(|file| file.path == path)
}

#[test]
fn an_nfd_name_matches_its_precomposed_tree_entry() {
    let fx = Fixture::standard();
    // Set explicitly: `git init` turns it on only on macOS.
    fx.git(&["config", "core.precomposeUnicode", "true"]);
    fx.write("caf\u{e9}.txt", b"x\n");
    fx.commit("add an accented name", "2024-01-03T00:00:00Z");
    fx.remove("caf\u{e9}.txt");
    // How Finder and Foundation spell the same name: `e` plus a combining acute accent.
    fx.write("cafe\u{301}.txt", b"x\n");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    assert_eq!(status_paths(&report), []);

    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    assert_eq!(listing.missing, []);
    let cafe = listing
        .entries
        .iter()
        .find(|entry| entry.name == "caf\u{e9}.txt")
        .unwrap();
    assert_eq!(cafe.status, WorktreeStatus::Unchanged);
    assert!(
        !listing
            .entries
            .iter()
            .any(|entry| entry.name == "cafe\u{301}.txt")
    );

    let files = repo.worktree_files(Some(tree), 10_000).unwrap();
    assert!(has_file(&files, "caf\u{e9}.txt"));
    assert!(!has_file(&files, "cafe\u{301}.txt"));
}

#[test]
fn an_nfd_name_stays_untracked_and_precomposed_when_new() {
    let fx = Fixture::standard();
    fx.git(&["config", "core.precomposeUnicode", "true"]);
    fx.write("cafe\u{301}.txt", b"x\n");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    assert_eq!(
        status_paths(&report),
        [("caf\u{e9}.txt".to_owned(), WorktreeStatus::Untracked)]
    );
}

/// Needs a case-insensitive filesystem, so that the renamed file still opens under its old
/// name; macOS's default APFS volume is one.
#[cfg(target_os = "macos")]
#[test]
fn a_case_only_rename_is_clean_under_ignore_case() {
    let fx = Fixture::standard();
    fx.git(&["config", "core.ignoreCase", "true"]);
    let readme = std::fs::read(fx.path().join("README.md")).unwrap();
    fx.remove("README.md");
    fx.write("readme.md", &readme);

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(Some(tree), 10_000).unwrap();
    assert_eq!(status_paths(&report), []);

    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    assert_eq!(listing.missing, []);
    let readme = listing
        .entries
        .iter()
        .find(|entry| entry.name == "README.md")
        .unwrap();
    assert_eq!(readme.status, WorktreeStatus::Unchanged);

    let files = repo.worktree_files(Some(tree), 10_000).unwrap();
    assert!(has_file(&files, "README.md"));
    assert!(!has_file(&files, "readme.md"));
}

/// Set only in the child process [`the_global_ignore_file_ranks_below_the_repository_rules`]
/// spawns: the repository to scan.
const GLOBAL_IGNORE_REPO: &str = "RC_GIT_TEST_GLOBAL_IGNORE_REPO";
/// The untracked paths the child must find, comma-separated.
const GLOBAL_IGNORE_EXPECT: &str = "RC_GIT_TEST_GLOBAL_IGNORE_EXPECT";

/// Untracked paths in `repo`, in status order.
fn untracked_paths(repo: &std::ffi::OsStr) -> Result<Vec<String>, Error> {
    let repo = Repository::open(repo)?;
    let tree = repo.resolve_tree("HEAD")?;
    let report = repo.worktree_status(Some(tree), 10_000)?;
    Ok(report
        .entries
        .iter()
        .filter(|entry| entry.status == WorktreeStatus::Untracked)
        .map(|entry| entry.path.to_string())
        .collect())
}

#[test]
fn the_global_ignore_file_ranks_below_the_repository_rules() {
    if let Some(repo) = std::env::var_os(GLOBAL_IGNORE_REPO) {
        let expected = std::env::var(GLOBAL_IGNORE_EXPECT).unwrap();
        let expected: Vec<&str> = expected.split(',').filter(|p| !p.is_empty()).collect();
        assert_eq!(untracked_paths(&repo).unwrap(), expected);
        return;
    }
    let fx = Fixture::standard();
    fx.write(".gitignore", b"!keep.log\n");
    fx.git(&["add", ".gitignore"]);
    fx.git(&["commit", "-q", "-m", "keep logs named keep"]);
    fx.write("debug.log", b"l\n");
    fx.write("keep.log", b"k\n");
    // Outside the walk: nothing under `.git` is ever listed.
    fx.write(".git/test-home/.config/git/ignore", b"*.log\n");
    fx.write(".git/test-xdg/git/ignore", b"*.nothing\n");
    let home = fx.path().join(".git/test-home");
    let xdg = fx.path().join(".git/test-xdg");
    let repo = fx.path().as_os_str();
    let run = |expect: &str, env: &[(&str, &std::ffi::OsStr)]| {
        let mut env = env.to_vec();
        env.push((GLOBAL_IGNORE_REPO, repo));
        env.push((GLOBAL_IGNORE_EXPECT, std::ffi::OsStr::new(expect)));
        rc_git_testkit::run_test_with_env(
            "the_global_ignore_file_ranks_below_the_repository_rules",
            &env,
        );
    };

    // `$HOME/.config/git/ignore` hides `debug.log`; the repository's `!keep.log` still wins.
    run("keep.log", &[("HOME", home.as_os_str())]);
    // `$XDG_CONFIG_HOME/git/ignore` replaces the `$HOME` one, as in git.
    run(
        "debug.log,keep.log",
        &[
            ("HOME", home.as_os_str()),
            ("XDG_CONFIG_HOME", xdg.as_os_str()),
        ],
    );
    // No home at all: only the repository's own rules apply.
    run("debug.log,keep.log", &[]);
}

/// `worktree_status` against `HEAD`, run on another thread so a blocking ignore-file read fails
/// the test after five seconds instead of hanging it.
fn status_within_five_seconds(path: &std::path::Path) -> Result<rc_git_core::StatusReport, String> {
    let path = path.to_path_buf();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let report = Repository::open(&path).and_then(|repo| {
            let tree = repo.resolve_tree("HEAD")?;
            repo.worktree_status(Some(tree), 10_000)
        });
        sender.send(report)
    });
    match receiver.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(report) => report.map_err(|error| error.to_string()),
        Err(error) => Err(format!(
            "worktree_status did not return within 5 s: {error}"
        )),
    }
}

#[test]
fn a_core_excludes_file_outside_the_repository_is_not_applied() {
    let fx = Fixture::standard();
    let outside = Fixture::empty_dir();
    outside.write("patterns", b"secret*.txt\n");
    let patterns = outside.path().join("patterns");
    fx.git(&["config", "core.excludesFile", patterns.to_str().unwrap()]);
    fx.write("secret1.txt", b"s\n");

    let report = status_within_five_seconds(fx.path()).unwrap();
    assert_eq!(
        status_paths(&report),
        [("secret1.txt".to_owned(), WorktreeStatus::Untracked)]
    );
}

#[test]
fn ignore_files_that_are_not_regular_files_are_skipped_without_blocking() {
    let fx = Fixture::standard();
    let outside = Fixture::empty_dir();
    outside.mkfifo("fifo");
    let fifo = outside.path().join("fifo");
    fx.git(&["config", "core.excludesFile", fifo.to_str().unwrap()]);
    fx.remove(".git/info/exclude");
    fx.mkfifo(".git/info/exclude");
    fx.write("piped/kept.txt", b"k\n");
    fx.mkfifo("piped/.gitignore");
    fx.write("zero/kept.txt", b"k\n");
    fx.symlink("/dev/zero", "zero/.gitignore");

    let report = status_within_five_seconds(fx.path()).unwrap();
    let untracked: Vec<String> = status_paths(&report)
        .into_iter()
        .filter(|(_, status)| *status == WorktreeStatus::Untracked)
        .map(|(path, _)| path)
        .collect();
    assert!(
        untracked.contains(&"piped/kept.txt".to_owned()),
        "{untracked:?}"
    );
    assert!(
        untracked.contains(&"zero/kept.txt".to_owned()),
        "{untracked:?}"
    );
    for skipped in [".git/info/exclude", "piped/.gitignore", "zero/.gitignore"] {
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains(skipped) && warning.contains("not a regular file")),
            "{skipped}: {:?}",
            report.warnings
        );
    }
}

#[test]
fn a_core_excludes_file_under_tilde_keeps_the_gitignore_rules() {
    let fx = Fixture::standard();
    fx.write(".gitignore", b"build/\n");
    fx.git(&["add", ".gitignore"]);
    fx.git(&["commit", "-q", "-m", "ignore build"]);
    fx.write("build/out.o", b"o\n");
    fx.git(&["config", "core.excludesFile", "~/no-such-file"]);

    let report = status_within_five_seconds(fx.path()).unwrap();
    assert!(
        status_paths(&report)
            .iter()
            .all(|(path, status)| !path.starts_with("build") || *status == WorktreeStatus::Ignored),
        "{:?}",
        status_paths(&report)
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}
