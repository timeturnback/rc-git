//! Tree diffs, blob diffs, and blame, against the standard fixture and a purpose-built one.
#![expect(clippy::unwrap_used, reason = "test helpers panic on fixture failure")]

use rc_git_core::{
    BlobDiff, ChangeKind, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT, DiffLineKind, EntryKind, Error,
    Oid, Repository,
};
use rc_git_testkit::Fixture;

fn oid(hex: &str) -> Oid {
    Oid::from_hex(hex).unwrap()
}

/// Two commits: the second modifies `a.txt`, renames `keep.txt` to `kept.txt`, and deletes
/// `gone.txt`.
fn diff_fixture() -> Fixture {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("a.txt", b"one\ntwo\nthree\n");
    fx.write(
        "keep.txt",
        b"unchanged content that is long enough to track\n",
    );
    fx.write("gone.txt", b"bye\n");
    fx.commit("first", "2024-01-01T00:00:00+0000");
    fx.write("a.txt", b"one\nTWO\nthree\nfour\n");
    fx.git(&["mv", "keep.txt", "kept.txt"]);
    fx.git(&["rm", "-q", "gone.txt"]);
    fx.commit("second", "2024-01-02T00:00:00+0000");
    fx
}

#[test]
fn tree_diff_reports_additions_between_fixture_commits() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let old = repo.resolve("v0.9").unwrap();
    let new = repo.resolve("main").unwrap();

    let changes = repo.diff_trees(Some(old), new).unwrap();
    let paths: Vec<_> = changes.iter().map(|c| c.path.to_string()).collect();
    assert_eq!(
        paths,
        [
            "Zeta.txt",
            "alpha.txt",
            "docs/guide.md",
            "link",
            "src/lib.rs"
        ]
    );
    assert!(changes.iter().all(|c| c.kind == ChangeKind::Added));
    assert!(changes.iter().all(|c| c.old.is_none() && c.new.is_some()));
    let link = changes.iter().find(|c| c.path == "link").unwrap();
    assert_eq!(link.entry, EntryKind::Symlink);

    let reversed = repo.diff_trees(Some(new), old).unwrap();
    assert!(reversed.iter().all(|c| c.kind == ChangeKind::Deleted));
    assert!(reversed.iter().all(|c| c.old.is_some() && c.new.is_none()));

    assert_eq!(repo.diff_trees(Some(new), new).unwrap().len(), 0);

    let root = repo.resolve("v0.9").unwrap();
    let from_nothing = repo.diff_trees(None, root).unwrap();
    assert_eq!(
        from_nothing.len(),
        3,
        "a root commit diffs against the empty tree"
    );
    assert!(from_nothing.iter().all(|c| c.kind == ChangeKind::Added));
}

#[test]
fn tree_diff_detects_modifications_renames_and_deletions() {
    let fx = diff_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let old = repo.resolve("main~1").unwrap();
    let new = repo.resolve("main").unwrap();

    let changes = repo.diff_trees(Some(old), new).unwrap();
    assert_eq!(changes.len(), 3);

    let modified = changes.iter().find(|c| c.path == "a.txt").unwrap();
    assert_eq!(modified.kind, ChangeKind::Modified);
    assert_eq!(modified.old.unwrap(), oid(&fx.rev_parse("main~1:a.txt")));
    assert_eq!(modified.new.unwrap(), oid(&fx.rev_parse("main:a.txt")));

    let renamed = changes.iter().find(|c| c.path == "kept.txt").unwrap();
    assert_eq!(renamed.kind, ChangeKind::Renamed);
    assert_eq!(
        renamed.source_path.as_ref().unwrap().to_string(),
        "keep.txt"
    );

    let deleted = changes.iter().find(|c| c.path == "gone.txt").unwrap();
    assert_eq!(deleted.kind, ChangeKind::Deleted);
}

#[test]
fn tree_diff_peels_and_rejects_non_trees() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let by_tag = repo
        .diff_trees(
            Some(repo.resolve("v0.9").unwrap()),
            repo.resolve("v1.0").unwrap(),
        )
        .unwrap();
    assert_ne!(by_tag.len(), 0, "tags peel to their trees");

    let blob = repo.resolve("main:README.md").unwrap();
    let err = repo
        .diff_trees(Some(blob), repo.resolve("main").unwrap())
        .unwrap_err();
    assert!(matches!(err, Error::WrongKind { .. }), "{err}");
}

#[test]
fn a_failed_tree_diff_names_the_compared_tree() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("d/a.txt", b"a\n");
    fx.commit("one", "2024-01-01T00:00:00+0000");
    fx.write("d/b.txt", b"b\n");
    fx.commit("two", "2024-01-02T00:00:00+0000");
    let subtree = fx.rev_parse("main:d");
    fx.remove(&format!(".git/objects/{}/{}", &subtree[..2], &subtree[2..]));

    let repo = Repository::open(fx.path()).unwrap();
    let to = oid(&fx.rev_parse("main^{tree}"));
    let err = repo
        .diff_trees(Some(repo.resolve("main~1").unwrap()), to)
        .unwrap_err();
    assert!(
        matches!(err, Error::Object { op: "diff trees", oid, .. } if oid == to),
        "{err}"
    );
}

#[test]
fn blob_diff_produces_unified_hunks() {
    let fx = diff_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let old = oid(&fx.rev_parse("main~1:a.txt"));
    let new = oid(&fx.rev_parse("main:a.txt"));

    let BlobDiff::Text { hunks } = repo
        .diff_blobs(
            Some(old),
            Some(new),
            DEFAULT_BLOB_LIMIT,
            DEFAULT_DIFF_CONTEXT,
        )
        .unwrap()
    else {
        panic!("expected a text diff");
    };
    assert_eq!(hunks.len(), 1);
    let hunk = &hunks[0];
    assert_eq!(
        (
            hunk.old_start,
            hunk.old_lines,
            hunk.new_start,
            hunk.new_lines
        ),
        (1, 3, 1, 4)
    );
    let lines: Vec<_> = hunk
        .lines
        .iter()
        .map(|l| (l.kind, l.text.to_string()))
        .collect();
    assert_eq!(
        lines,
        [
            (DiffLineKind::Context, "one".to_owned()),
            (DiffLineKind::Removed, "two".to_owned()),
            (DiffLineKind::Added, "TWO".to_owned()),
            (DiffLineKind::Context, "three".to_owned()),
            (DiffLineKind::Added, "four".to_owned()),
        ]
    );
}

#[test]
fn blob_diff_handles_absent_sides_binary_and_limits() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let readme = oid(&fx.rev_parse("main:README.md"));
    let bin = oid(&fx.rev_parse("main:bin.dat"));

    let BlobDiff::Text { hunks } = repo
        .diff_blobs(None, Some(readme), DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)
        .unwrap()
    else {
        panic!("expected a text diff for an added file");
    };
    assert_eq!(hunks.len(), 1);
    assert!(hunks[0].lines.iter().all(|l| l.kind == DiffLineKind::Added));
    assert_eq!(hunks[0].old_lines, 0);

    let identical = repo
        .diff_blobs(
            Some(readme),
            Some(readme),
            DEFAULT_BLOB_LIMIT,
            DEFAULT_DIFF_CONTEXT,
        )
        .unwrap();
    assert_eq!(identical, BlobDiff::Text { hunks: vec![] });

    let BlobDiff::Binary { old_size, new_size } = repo
        .diff_blobs(
            Some(bin),
            Some(readme),
            DEFAULT_BLOB_LIMIT,
            DEFAULT_DIFF_CONTEXT,
        )
        .unwrap()
    else {
        panic!("expected binary");
    };
    assert!(old_size > 0 && new_size > 0);

    let BlobDiff::TooLarge { size, limit } = repo
        .diff_blobs(Some(readme), Some(readme), 3, DEFAULT_DIFF_CONTEXT)
        .unwrap()
    else {
        panic!("expected too large");
    };
    assert_eq!((size, limit), (17, 3));

    let tree = repo.resolve_tree("main").unwrap();
    let err = repo
        .diff_blobs(
            Some(tree),
            Some(readme),
            DEFAULT_BLOB_LIMIT,
            DEFAULT_DIFF_CONTEXT,
        )
        .unwrap_err();
    assert!(matches!(err, Error::WrongKind { .. }), "{err}");
}

#[test]
fn a_diff_over_the_limit_reports_the_larger_side_from_the_headers() {
    let fx = Fixture::standard();
    fx.write("big.txt", &[b'x'; 40]);
    let big = oid(fx.git_output(&["hash-object", "-w", "big.txt"]).trim());
    let repo = Repository::open(fx.path()).unwrap();
    let readme = oid(&fx.rev_parse("HEAD:README.md"));

    let diff = repo
        .diff_blobs(Some(readme), Some(big), 10, DEFAULT_DIFF_CONTEXT)
        .unwrap();
    assert_eq!(
        diff,
        BlobDiff::TooLarge {
            size: 40,
            limit: 10
        }
    );

    let diff = repo
        .diff_worktree_file(Some(readme), "big.txt", 10, DEFAULT_DIFF_CONTEXT)
        .unwrap();
    assert_eq!(
        diff,
        BlobDiff::TooLarge {
            size: 40,
            limit: 10
        }
    );
}

#[test]
fn blame_attributes_lines_to_their_commits() {
    let fx = diff_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let first = oid(&fx.rev_parse("main~1"));
    let second = oid(&fx.rev_parse("main"));

    let ranges = repo.blame(tip, "a.txt").unwrap();
    let mut lines = vec![Oid::from_hex(&"0".repeat(40)).unwrap(); 4];
    for range in &ranges {
        for line in range.start_line..range.start_line + range.line_count {
            lines[line as usize - 1] = range.commit;
        }
    }
    assert_eq!(lines, [first, second, first, second]);

    let renamed = repo.blame(tip, "kept.txt").unwrap();
    assert_eq!(renamed.len(), 1);
    assert_eq!(
        renamed[0].commit, second,
        "without rename tracking the rename commit owns the lines"
    );
}

#[test]
fn blame_reports_missing_paths_and_bad_tips() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let err = repo.blame(tip, "no/such/file").unwrap_err();
    assert!(matches!(err, Error::Object { .. }), "{err}");

    let blob = repo.resolve("main:README.md").unwrap();
    assert!(repo.blame(blob, "README.md").is_err());
}

/// Three commits, each appending one line to `a.txt`: `one`, `two`, `three`.
fn append_fixture() -> Fixture {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("a.txt", b"1\n");
    fx.commit("one", "2024-01-01T00:00:00+0000");
    fx.write("a.txt", b"1\n2\n");
    fx.commit("two", "2024-01-02T00:00:00+0000");
    fx.write("a.txt", b"1\n2\n3\n");
    fx.commit("three", "2024-01-03T00:00:00+0000");
    fx
}

/// The commit of each line, in line order.
fn line_owners(ranges: &[rc_git_core::BlameRange]) -> Vec<Oid> {
    let mut owners = Vec::new();
    for range in ranges {
        owners.extend(std::iter::repeat_n(range.commit, range.line_count as usize));
    }
    owners
}

#[test]
fn blame_stops_at_the_shallow_boundary() {
    let origin = append_fixture();
    let clone = Fixture::empty_dir();
    let url = format!("file://{}", origin.path().display());
    clone.git(&["clone", "-q", "--depth", "2", &url, "."]);
    let repo = Repository::open(clone.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let boundary = oid(&clone.rev_parse("main~1"));

    let ranges = repo.blame(tip, "a.txt").unwrap();
    assert_eq!(
        line_owners(&ranges),
        [boundary, boundary, tip],
        "the boundary keeps the lines older than the clone, as `git blame` does"
    );
}

#[test]
fn blame_names_the_missing_commit() {
    let fx = append_fixture();
    let root = fx.rev_parse("main~2");
    fx.remove(&format!(".git/objects/{}/{}", &root[..2], &root[2..]));
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let err = repo.blame(tip, "a.txt").unwrap_err();
    let Error::Object { oid: culprit, .. } = err else {
        panic!("expected an object error, got {err}");
    };
    assert_eq!(
        culprit,
        oid(&root),
        "the error names the missing commit, not the tip"
    );
}

#[test]
fn worktree_diff_shows_disk_edits_against_the_blob() {
    let fx = Fixture::standard();
    fx.write("alpha.txt", b"alpha\nbeta\n");
    let repo = Repository::open(fx.path()).unwrap();
    let old = oid(&fx.rev_parse("HEAD:alpha.txt"));
    let BlobDiff::Text { hunks } = repo
        .diff_worktree_file(
            Some(old),
            "alpha.txt",
            DEFAULT_BLOB_LIMIT,
            DEFAULT_DIFF_CONTEXT,
        )
        .unwrap()
    else {
        panic!("expected a text diff");
    };
    assert_eq!(hunks.len(), 1);
    let added: Vec<String> = hunks[0]
        .lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Added)
        .map(|line| line.text.to_string())
        .collect();
    assert_eq!(added, ["beta"]);
}

#[test]
fn worktree_diff_of_an_untracked_file_is_pure_additions() {
    let fx = Fixture::standard();
    fx.write("new.txt", b"one\ntwo\n");
    let repo = Repository::open(fx.path()).unwrap();
    let BlobDiff::Text { hunks } = repo
        .diff_worktree_file(None, "new.txt", DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)
        .unwrap()
    else {
        panic!("expected a text diff");
    };
    assert!(
        hunks[0]
            .lines
            .iter()
            .all(|line| line.kind == DiffLineKind::Added)
    );
    assert_eq!(hunks[0].lines.len(), 2);
}

#[test]
fn worktree_diff_reports_binary_size_limits_and_missing_files() {
    let fx = Fixture::standard();
    fx.write("bin.dat", b"\x89PNG\0\0changed");
    let repo = Repository::open(fx.path()).unwrap();
    let old = oid(&fx.rev_parse("HEAD:bin.dat"));
    let diff = repo
        .diff_worktree_file(
            Some(old),
            "bin.dat",
            DEFAULT_BLOB_LIMIT,
            DEFAULT_DIFF_CONTEXT,
        )
        .unwrap();
    assert!(matches!(diff, BlobDiff::Binary { .. }), "{diff:?}");

    let readme = oid(&fx.rev_parse("HEAD:README.md"));
    let diff = repo
        .diff_worktree_file(Some(readme), "README.md", 3, DEFAULT_DIFF_CONTEXT)
        .unwrap();
    assert!(
        matches!(diff, BlobDiff::TooLarge { limit: 3, .. }),
        "{diff:?}"
    );

    let err = repo
        .diff_worktree_file(
            None,
            "missing.txt",
            DEFAULT_BLOB_LIMIT,
            DEFAULT_DIFF_CONTEXT,
        )
        .unwrap_err();
    assert!(matches!(err, Error::Worktree { .. }), "{err}");
}
