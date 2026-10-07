//! Tree diffs, blob diffs, and blame, against the standard fixture and a purpose-built one.
#![expect(clippy::unwrap_used, reason = "test helpers panic on fixture failure")]

use rc_git_core::{
    BlobDiff, ChangeKind, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT, DiffHunk, DiffLineKind,
    EntryKind, Error, LineEnd, ObjectKind, Oid, Repository,
};
use rc_git_testkit::{BIN_DAT, Fixture, README};

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

/// Eight numbered lines, the last one tagged so two files can differ only there.
fn eight_lines(name: &str, last: &str) -> Vec<u8> {
    let mut text = Vec::new();
    for n in 1..8 {
        text.extend_from_slice(format!("{name} line {n}\n").as_bytes());
    }
    text.extend_from_slice(format!("{name} {last}\n").as_bytes());
    text
}

/// Two commits under `diff.renames=copies`: the second renames `keep.txt` to `kept.txt` with
/// its last line changed, copies `src.txt` to `dup.txt`, and changes `src.txt`'s last line.
fn rename_and_copy_fixture() -> Fixture {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("keep.txt", &eight_lines("keep", "end"));
    fx.write("src.txt", &eight_lines("src", "end"));
    fx.commit("first", "2024-01-01T00:00:00+0000");
    fx.git(&["mv", "keep.txt", "kept.txt"]);
    fx.write("kept.txt", &eight_lines("keep", "edited"));
    fx.write("dup.txt", &eight_lines("src", "end"));
    fx.write("src.txt", &eight_lines("src", "edited"));
    fx.commit("second", "2024-01-02T00:00:00+0000");
    fx.git(&["config", "diff.renames", "copies"]);
    fx
}

#[test]
fn tree_diff_keeps_the_source_of_a_copy() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("src.txt", &eight_lines("src", "end"));
    fx.commit("first", "2024-01-01T00:00:00+0000");
    fx.write("dup.txt", &eight_lines("src", "end"));
    fx.write("src.txt", &eight_lines("src", "edited"));
    fx.commit("second", "2024-01-02T00:00:00+0000");
    fx.git(&["config", "diff.renames", "copies"]);
    let repo = Repository::open(fx.path()).unwrap();

    let changes = repo
        .diff_trees(
            Some(repo.resolve("main~1").unwrap()),
            repo.resolve("main").unwrap(),
        )
        .unwrap();

    assert_eq!(changes.len(), 2, "{changes:?}");
    let source = changes.iter().find(|c| c.path == "src.txt").unwrap();
    assert_eq!(source.kind, ChangeKind::Modified);
    assert_eq!(source.old.unwrap(), oid(&fx.rev_parse("main~1:src.txt")));
    assert_eq!(source.new.unwrap(), oid(&fx.rev_parse("main:src.txt")));
    let copy = changes.iter().find(|c| c.path == "dup.txt").unwrap();
    assert_eq!(copy.kind, ChangeKind::Added);
    assert_eq!(copy.source_path, None);
    assert_eq!(copy.new.unwrap(), oid(&fx.rev_parse("main:dup.txt")));
}

#[test]
fn tree_diff_reports_rename_with_edit_and_copy() {
    let fx = rename_and_copy_fixture();
    let repo = Repository::open(fx.path()).unwrap();

    let changes = repo
        .diff_trees(
            Some(repo.resolve("main~1").unwrap()),
            repo.resolve("main").unwrap(),
        )
        .unwrap();

    let paths: Vec<_> = changes.iter().map(|c| c.path.to_string()).collect();
    assert_eq!(paths, ["dup.txt", "kept.txt", "src.txt"]);
    let renamed = changes.iter().find(|c| c.path == "kept.txt").unwrap();
    assert_eq!(renamed.kind, ChangeKind::Renamed);
    assert_eq!(
        renamed.source_path.as_ref().unwrap().to_string(),
        "keep.txt"
    );
    assert_eq!(renamed.old.unwrap(), oid(&fx.rev_parse("main~1:keep.txt")));
    assert_eq!(renamed.new.unwrap(), oid(&fx.rev_parse("main:kept.txt")));
    assert_ne!(renamed.old, renamed.new);
    let source = changes.iter().find(|c| c.path == "src.txt").unwrap();
    assert_eq!(source.kind, ChangeKind::Modified);
    let copy = changes.iter().find(|c| c.path == "dup.txt").unwrap();
    assert_eq!(copy.kind, ChangeKind::Added);
}

#[test]
fn tree_diff_without_renames_splits_a_rename() {
    let fx = rename_and_copy_fixture();
    let repo = Repository::open(fx.path()).unwrap();

    let changes = repo
        .diff_trees_without_renames(
            Some(repo.resolve("main~1").unwrap()),
            repo.resolve("main").unwrap(),
        )
        .unwrap();

    let summary: Vec<_> = changes
        .iter()
        .map(|c| (c.path.to_string(), c.kind, c.source_path.is_some()))
        .collect();
    assert_eq!(
        summary,
        [
            ("dup.txt".to_owned(), ChangeKind::Added, false),
            ("keep.txt".to_owned(), ChangeKind::Deleted, false),
            ("kept.txt".to_owned(), ChangeKind::Added, false),
            ("src.txt".to_owned(), ChangeKind::Modified, false),
        ]
    );
    let deleted = changes.iter().find(|c| c.path == "keep.txt").unwrap();
    assert_eq!(deleted.old.unwrap(), oid(&fx.rev_parse("main~1:keep.txt")));
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
    assert!(
        matches!(
            err,
            Error::WrongKind {
                expected: ObjectKind::Tree,
                actual: ObjectKind::Blob,
                ..
            }
        ),
        "{err}"
    );
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

/// Runs `diff_blobs` on `path` between `main~1` and `main` and returns the text hunks.
#[expect(clippy::panic, reason = "test helper panics on a non-text diff")]
fn text_hunks(fx: &Fixture, path: &str, context: u32) -> Vec<DiffHunk> {
    let repo = Repository::open(fx.path()).unwrap();
    let old = oid(&fx.rev_parse(&format!("main~1:{path}")));
    let new = oid(&fx.rev_parse(&format!("main:{path}")));
    let BlobDiff::Text { hunks } = repo
        .diff_blobs(Some(old), Some(new), DEFAULT_BLOB_LIMIT, context)
        .unwrap()
    else {
        panic!("expected a text diff of {path}");
    };
    hunks
}

fn hunk_ranges(hunks: &[DiffHunk]) -> Vec<(u32, u32, u32, u32)> {
    hunks
        .iter()
        .map(|h| (h.old_start, h.old_lines, h.new_start, h.new_lines))
        .collect()
}

#[test]
fn huge_context_is_clamped() {
    let fx = diff_fixture();
    let hunks = text_hunks(&fx, "a.txt", u32::MAX);
    assert_eq!(hunk_ranges(&hunks), [(1, 3, 1, 4)]);

    let two = Fixture::empty_dir();
    two.git(&["init", "-q", "-b", "main"]);
    two.write("b.txt", b"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
    two.commit("first", "2024-01-01T00:00:00+0000");
    two.write("b.txt", b"ONE\n2\n3\n4\n5\n6\n7\n8\n9\nTEN\n");
    two.commit("second", "2024-01-02T00:00:00+0000");
    assert_eq!(hunk_ranges(&text_hunks(&two, "b.txt", 1)).len(), 2);
    let hunks = text_hunks(&two, "b.txt", 1 << 31);
    assert_eq!(hunk_ranges(&hunks), [(1, 10, 1, 10)]);
}

fn numbered_lines(edited: &[u32]) -> Vec<u8> {
    let mut text = Vec::new();
    for n in 1..=20 {
        let line = if edited.contains(&n) {
            format!("EDITED {n}\n")
        } else {
            format!("line {n}\n")
        };
        text.extend_from_slice(line.as_bytes());
    }
    text
}

#[test]
fn context_size_controls_hunk_splitting() {
    let fx = two_versions("f.txt", &numbered_lines(&[]), &numbered_lines(&[2, 18]));

    let hunks = text_hunks(&fx, "f.txt", 3);
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunk_ranges(&hunks), [(1, 5, 1, 5), (15, 6, 15, 6)]);

    let hunks = text_hunks(&fx, "f.txt", 10);
    assert_eq!(hunk_ranges(&hunks), [(1, 20, 1, 20)]);

    let hunks = text_hunks(&fx, "f.txt", 0);
    assert_eq!(hunk_ranges(&hunks), [(2, 1, 2, 1), (18, 1, 18, 1)]);
    assert!(
        hunks
            .iter()
            .flat_map(|h| &h.lines)
            .all(|l| l.kind != DiffLineKind::Context),
        "{hunks:?}"
    );
}

/// One file committed twice on `main`, `old` then `new`.
fn two_versions(path: &str, old: &[u8], new: &[u8]) -> Fixture {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write(path, old);
    fx.commit("first", "2024-01-01T00:00:00+0000");
    fx.write(path, new);
    fx.commit("second", "2024-01-02T00:00:00+0000");
    fx
}

fn line_summary(hunks: &[DiffHunk]) -> Vec<(DiffLineKind, String, LineEnd)> {
    hunks
        .iter()
        .flat_map(|h| &h.lines)
        .map(|l| (l.kind, l.text.to_string(), l.eol))
        .collect()
}

#[test]
fn adding_a_final_newline_marks_the_old_line_unterminated() {
    let fx = two_versions("f.txt", b"x\na", b"x\na\n");
    assert_eq!(
        line_summary(&text_hunks(&fx, "f.txt", DEFAULT_DIFF_CONTEXT)),
        [
            (DiffLineKind::Context, "x".to_owned(), LineEnd::Lf),
            (DiffLineKind::Removed, "a".to_owned(), LineEnd::None),
            (DiffLineKind::Added, "a".to_owned(), LineEnd::Lf),
        ]
    );
}

#[test]
fn lf_to_crlf_marks_the_added_lines_crlf() {
    let fx = two_versions("f.txt", b"x\na\n", b"x\r\na\r\n");
    assert_eq!(
        line_summary(&text_hunks(&fx, "f.txt", DEFAULT_DIFF_CONTEXT)),
        [
            (DiffLineKind::Removed, "x".to_owned(), LineEnd::Lf),
            (DiffLineKind::Removed, "a".to_owned(), LineEnd::Lf),
            (DiffLineKind::Added, "x".to_owned(), LineEnd::CrLf),
            (DiffLineKind::Added, "a".to_owned(), LineEnd::CrLf),
        ]
    );
}

#[test]
fn a_trailing_carriage_return_without_newline_stays_in_the_text() {
    let fx = two_versions("f.txt", b"a\n", b"a\r");
    assert_eq!(
        line_summary(&text_hunks(&fx, "f.txt", DEFAULT_DIFF_CONTEXT)),
        [
            (DiffLineKind::Removed, "a".to_owned(), LineEnd::Lf),
            (DiffLineKind::Added, "a\r".to_owned(), LineEnd::None),
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
    assert_eq!(
        hunks[0].old_start, 0,
        "an empty side starts at 0, as in git's header"
    );
    assert_eq!(hunks[0].new_start, 1);

    let BlobDiff::Text { hunks } = repo
        .diff_blobs(Some(readme), None, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)
        .unwrap()
    else {
        panic!("expected a text diff for a deleted file");
    };
    assert_eq!(
        (hunks[0].old_start, hunks[0].new_start, hunks[0].new_lines),
        (1, 0, 0)
    );

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
    assert_eq!(
        (old_size, new_size),
        (BIN_DAT.len() as u64, README.len() as u64)
    );

    assert_eq!(
        repo.diff_blobs(None, Some(bin), DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)
            .unwrap(),
        BlobDiff::Binary {
            old_size: 0,
            new_size: BIN_DAT.len() as u64
        },
        "an absent side counts as size 0"
    );

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
    assert!(
        matches!(
            err,
            Error::WrongKind {
                expected: ObjectKind::Blob,
                actual: ObjectKind::Tree,
                ..
            }
        ),
        "{err}"
    );
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
    assert!(
        matches!(&err, Error::PathNotFound { path, commit, .. }
            if path == "no/such/file" && *commit == tip),
        "{err}"
    );
    let msg = err.to_string();
    assert!(msg.contains("no/such/file"), "{msg}");
    assert!(!msg.contains("missing or unreadable"), "{msg}");

    let err = repo.blame(tip, "src").unwrap_err();
    assert!(
        matches!(
            err,
            Error::WrongKind {
                op: "blame file",
                expected: ObjectKind::Blob,
                actual: ObjectKind::Tree,
                ..
            }
        ),
        "{err}"
    );

    let tag = repo.resolve("v1.0").unwrap();
    assert_ne!(tag, tip, "v1.0 is an annotated tag");
    assert_eq!(
        repo.blame(tag, "README.md").unwrap(),
        repo.blame(repo.resolve("v1.0^{commit}").unwrap(), "README.md")
            .unwrap()
    );

    let blob = repo.resolve("main:README.md").unwrap();
    let err = repo.blame(blob, "README.md").unwrap_err();
    assert!(
        matches!(
            err,
            Error::WrongKind {
                expected: ObjectKind::Commit,
                actual: ObjectKind::Blob,
                ..
            }
        ),
        "{err}"
    );
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
