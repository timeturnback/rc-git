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
    let listing = repo.worktree_entries(tree, "").unwrap();
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
    fx.remove("Zeta.txt");

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
    fx.remove("link");
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
    let report = repo.worktree_status(tree, 10_000)?;
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
    let (_bare, bare_path) = fx.bare_copy();
    let repo = Repository::open(&bare_path).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let err = repo.worktree_entries(tree, "").unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
    let err = repo
        .worktree_file("README.md", DEFAULT_BLOB_LIMIT)
        .unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
    let err = repo.worktree_status(tree, 10_000).unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
    let err = repo.worktree_files(tree, 10_000).unwrap_err();
    assert!(err.to_string().contains("bare"), "{err}");
}

#[test]
fn every_spelling_of_a_directory_lists_the_same_entries() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();

    let root = repo.worktree_entries(tree, "").unwrap();
    for spelling in ["/", ".", "./", "//"] {
        assert_eq!(
            repo.worktree_entries(tree, spelling).unwrap(),
            root,
            "root spelled {spelling:?}"
        );
    }
    let src = repo.worktree_entries(tree, "src").unwrap();
    for spelling in ["./src", "/src/", "src//", "./src/."] {
        assert_eq!(
            repo.worktree_entries(tree, spelling).unwrap(),
            src,
            "src spelled {spelling:?}"
        );
    }

    let err = repo.worktree_entries(tree, "src/..").unwrap_err();
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
        let err = repo.worktree_entries(tree, dir).unwrap_err();
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
    fx.remove("src/lib.rs");
    fx.remove("docs");

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
    fx.set_mode("alpha.txt", 0o755);
    fx.remove("link");
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
fn only_the_owner_exec_bit_counts_as_a_mode_change() {
    let fx = Fixture::standard();
    fx.set_mode("alpha.txt", 0o645);

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    assert_eq!(repo.worktree_status(tree, 10_000).unwrap().entries, []);
    let listing = repo.worktree_entries(tree, "").unwrap();
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
        .worktree_status(tree, 10_000)
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
    let listing = repo.worktree_entries(tree, "").unwrap();
    let alpha = listing
        .entries
        .iter()
        .find(|entry| entry.name == "alpha.txt")
        .unwrap();
    assert_eq!(
        (alpha.kind, alpha.status),
        (EntryKind::Blob, WorktreeStatus::Unchanged)
    );
    let files = repo.worktree_files(tree, 10_000).unwrap().files;
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
    fx.remove("docs");
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

/// The index is consulted as a stat cache to skip hashing unchanged files. It may only ever
/// save that hash, never answer in place of one, so every way an entry can stop describing
/// what is on disk has to fall back to reading the file.
#[test]
fn the_index_stat_cache_never_vouches_for_a_changed_file() {
    let fx = Fixture::standard();
    // `git status` rewrites the index with a fresh stat for every file, which is exactly the
    // state the cache is built to exploit: without it every case below reads the file anyway
    // and the test cannot tell a working cache from a broken one.
    // A second must pass before the edits, so that the index written after them carries a
    // later timestamp than the stat of every file left alone. Without that gap every entry
    // is racy, the cache declines them all, and the test cannot tell it from a broken one.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    // Same size as the blob, so only content can decide.
    fx.write("alpha.txt", b"omega\n");
    // Rewritten with what it already held: the stat moves, the content does not.
    let readme = std::fs::read(fx.path().join("README.md")).unwrap();
    fx.write("README.md", &readme);
    // Staged, so the index now holds the new blob and its id no longer matches the tree's.
    fx.write("Zeta.txt", b"omega\n");
    fx.git(&["add", "Zeta.txt"]);

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(tree, 10_000).unwrap();
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

/// A file edited in the same second the index was written is indistinguishable from one
/// edited just after it, so the entry proves nothing and the file must be read.
#[test]
fn a_racy_index_entry_still_reports_the_edit() {
    let fx = Fixture::standard();
    fx.write("alpha.txt", b"omega\n");

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo.worktree_status(tree, 10_000).unwrap();
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
    let report = repo.worktree_status(tree, 10_000).unwrap();
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
    let list = repo.worktree_files(tree, 10_000).unwrap();
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
    let list = repo.worktree_files(tree, 10_000).unwrap();
    let vendored: Vec<String> = file_paths(&list)
        .into_iter()
        .filter(|path| path.starts_with("vendor/"))
        .collect();
    assert_eq!(vendored, ["vendor/extra.txt", "vendor/kept.txt"]);
}

#[test]
fn worktree_files_truncates_at_the_scan_cap() {
    let fx = Fixture::standard();
    for i in 0..20 {
        fx.write(&format!("many/file{i}.txt"), b"x\n");
    }

    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    let list = repo.worktree_files(tree, 5).unwrap();
    assert!(list.truncated);
    assert_eq!(list.scanned, 6);

    let full = repo.worktree_files(tree, 100_000).unwrap();
    assert!(!full.truncated);
    assert_eq!(full.files.len(), 28);
}

#[test]
fn worktree_files_reports_an_unreadable_directory() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    fx.remove("");
    let err = repo.worktree_files(tree, 10_000).unwrap_err();
    assert!(err.to_string().contains("list working tree files"), "{err}");
}
