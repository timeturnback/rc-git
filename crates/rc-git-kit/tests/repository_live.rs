//! Ported from the behaviour tests in `RepositoryIntegrationTests.swift`: `LiveRepository`
//! read through the `RepositoryReading` seam against the standard fixture. The FFI
//! marshalling those tests also pinned has no successor; core's own suites cover the rest.
#![expect(
    clippy::unwrap_used,
    reason = "the helpers open fixtures that always exist"
)]

use std::sync::Arc;
use std::thread;

use rc_git_core::{
    Blame, BlobContent, BlobDiff, ChangeKind, CommitFilter, ContentQuery, DEFAULT_BLOB_LIMIT,
    DEFAULT_DIFF_CONTEXT, DefaultBranch, DiffLine, DiffLineKind, EntryKind, FileSearch, LineEnd,
    Oid, RefKind, SearchLimits, WorktreeStatus,
};
use rc_git_kit::canonical_path;
use rc_git_kit::repository::{DEFAULT_STATUS_SCAN_CAP, LiveRepository, RepositoryReading};
use rc_git_testkit::{Fixture, MAIN_RS};

fn open(fixture: &Fixture) -> LiveRepository {
    LiveRepository::open(fixture.path()).unwrap()
}

fn rev(fixture: &Fixture, spec: &str) -> Oid {
    Oid::from_hex(&fixture.rev_parse(spec)).unwrap()
}

fn query(text: &str) -> ContentQuery {
    ContentQuery::new(text.as_bytes()).unwrap()
}

#[test]
fn opening_a_non_repository_fails_with_the_path() {
    let fixture = Fixture::empty_dir();
    let error = LiveRepository::open(fixture.path()).unwrap_err();
    let path = fixture.path().display().to_string();
    assert!(error.message().contains(&path), "{error}");
}

#[test]
fn the_common_directory_is_the_shared_git_dir_of_a_linked_worktree() {
    let fixture = Fixture::standard();
    let outside = Fixture::empty_dir();
    let main = open(&fixture);
    assert_eq!(main.common_dir(), main.git_dir());

    let linked_path = outside.path().join("linked");
    fixture.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "wt",
        linked_path.to_str().unwrap(),
    ]);
    let linked = LiveRepository::open(&linked_path).unwrap();
    let common = canonical_path::resolve(linked.common_dir());
    assert_eq!(common, canonical_path::resolve(&fixture.git_dir()));
    assert_ne!(canonical_path::resolve(linked.git_dir()), common);
}

#[test]
fn head_crosses_detached_and_unborn_and_a_bare_repository_has_no_working_directory() {
    let fixture = Fixture::standard();
    fixture.git(&["checkout", "-q", "--detach", "HEAD"]);
    let detached = open(&fixture).head().unwrap();
    assert_eq!(detached.ref_name, None);
    assert_eq!(detached.target, Some(rev(&fixture, "HEAD")));

    let unborn_fixture = Fixture::empty_dir();
    unborn_fixture.git(&["init", "-q", "-b", "work"]);
    let unborn = open(&unborn_fixture).head().unwrap();
    assert_eq!(unborn.target, None);
    assert_eq!(unborn.ref_name.as_deref(), Some("refs/heads/work"));

    let (_holder, bare) = fixture.bare_copy();
    assert_eq!(LiveRepository::open(&bare).unwrap().work_dir(), None);
}

#[test]
fn reads_refs_trees_blobs_and_history() {
    let fixture = Fixture::standard();
    let repo: Arc<dyn RepositoryReading> = Arc::new(open(&fixture));
    assert!(repo.work_dir().is_some());
    let head = repo.head().unwrap();
    assert_eq!(head.ref_name.as_deref(), Some("refs/heads/main"));
    assert!(head.exact_ref_name);
    assert_eq!(
        repo.default_branch().unwrap(),
        Some(DefaultBranch {
            name: "main".to_owned(),
            exact_name: true
        }),
        "the fixture has no remote HEAD, so the local mainline answers"
    );

    let listing = repo.refs().unwrap();
    assert!(listing.broken.is_empty());
    let tag = listing
        .refs
        .iter()
        .find(|r| r.short_name == "v1.0")
        .unwrap();
    assert_eq!(tag.kind, RefKind::Tag);
    assert_eq!(tag.target, rev(&fixture, "v1.0^{commit}"));

    let root = repo.resolve_tree("main").unwrap();
    assert_eq!(root, rev(&fixture, "main^{tree}"));
    assert_eq!(repo.tree_of(tag.target).unwrap(), root);
    let entries = repo.tree_entries(root).unwrap();
    assert_eq!(entries.len(), 7);
    assert_eq!(entries[0].kind, EntryKind::Tree);

    let main_rs = repo.resolve("main:src/main.rs").unwrap();
    let size = MAIN_RS.len() as u64;
    let BlobContent::Text { data, .. } = repo.read_blob(main_rs, DEFAULT_BLOB_LIMIT).unwrap()
    else {
        panic!("expected text");
    };
    assert_eq!(data, MAIN_RS);
    assert!(matches!(
        repo.read_blob(main_rs, size).unwrap(),
        BlobContent::Text { .. }
    ));
    assert_eq!(
        repo.read_blob(main_rs, size - 1).unwrap(),
        BlobContent::TooLarge {
            size,
            limit: size - 1
        }
    );

    let tip = repo.resolve("main").unwrap();
    let cursor = repo.log_cursor(tip).unwrap();
    let first = cursor.next(1).unwrap();
    assert_eq!(first[0].title, "Add docs and library");
    let rest = cursor.next(5).unwrap();
    assert_eq!(rest.len(), 1, "the cursor resumes");
    assert_eq!(repo.commit(rest[0].oid).unwrap(), rest[0]);
    assert!(cursor.next(5).unwrap().is_empty());

    assert!(repo.resolve("nope").is_err());
    assert!(repo.tree_entries(main_rs).is_err());
}

#[test]
fn walks_path_history_and_searches_with_a_scan_cursor() {
    let fixture = Fixture::standard();
    let repo = open(&fixture);
    let tip = repo.resolve("main").unwrap();
    let library = CommitFilter {
        path: Some("src/lib.rs".to_owned()),
        ..CommitFilter::default()
    };
    let page = repo
        .history_walk(tip, library)
        .unwrap()
        .next(10, 100)
        .unwrap();
    assert_eq!(page.commits.len(), 1);
    assert_eq!(page.scanned, 2);
    assert!(page.complete);

    let initial = CommitFilter {
        message: Some("Initial".to_owned()),
        ..CommitFilter::default()
    };
    let walk = repo.history_walk(tip, initial).unwrap();
    let first = walk.next(10, 1).unwrap();
    assert!(first.commits.is_empty());
    assert!(!first.complete);
    let rest = walk.next(10, 1).unwrap();
    assert_eq!(rest.commits[0].title, "Initial import", "the walk resumes");
    assert!(rest.complete);
}

#[test]
fn reads_commit_details() {
    let fixture = Fixture::standard();
    fixture.commit_with_body("Detail commit", "The body.", "2024-01-04T00:00:00+0000");
    let repo = open(&fixture);
    let tip = repo.resolve("main").unwrap();
    let detail = repo.commit_details(tip).unwrap();
    assert_eq!(detail.summary, repo.commit(tip).unwrap());
    assert_eq!(detail.body, "The body.");
    assert_eq!(detail.tree, rev(&fixture, "main^{tree}"));
    assert!(
        repo.commit_details(repo.resolve_tree("main").unwrap())
            .is_err()
    );
}

#[test]
fn diffs_trees_blobs_and_worktree_files_and_blames() {
    let fixture = Fixture::standard();
    let repo = open(&fixture);
    let old = repo.resolve("v0.9").unwrap();
    let new = repo.resolve("main").unwrap();
    let changes = repo.diff_trees(Some(old), new).unwrap();
    assert!(
        changes
            .iter()
            .all(|change| change.kind == ChangeKind::Added)
    );
    let guide = changes
        .iter()
        .find(|change| change.path == "docs/guide.md")
        .unwrap();
    let BlobDiff::Text { hunks } = repo
        .diff_blobs(None, guide.new, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)
        .unwrap()
    else {
        panic!("expected a text diff");
    };
    assert_eq!(
        hunks[0].lines,
        [DiffLine {
            kind: DiffLineKind::Added,
            text: "guide".into(),
            eol: LineEnd::Lf
        }]
    );
    assert_eq!(
        repo.diff_blobs(guide.new, guide.new, 2, DEFAULT_DIFF_CONTEXT)
            .unwrap(),
        BlobDiff::TooLarge { size: 6, limit: 2 }
    );

    let Blame::Ranges(ranges) = repo
        .blame(new, "README.md", DEFAULT_BLOB_LIMIT, None)
        .unwrap()
    else {
        panic!("expected ranges");
    };
    assert_eq!(ranges[0].commit, rev(&fixture, "v0.9^{commit}"));
    assert!(
        repo.blame(new, "nope.txt", DEFAULT_BLOB_LIMIT, None)
            .is_err()
    );

    fixture.write("alpha.txt", b"alpha\nbeta\n");
    let tree = repo.resolve_tree("HEAD").unwrap();
    let report = repo
        .worktree_status(Some(tree), DEFAULT_STATUS_SCAN_CAP)
        .unwrap();
    let modified = report
        .entries
        .iter()
        .find(|entry| entry.path == "alpha.txt")
        .unwrap();
    assert_eq!(modified.status, WorktreeStatus::Modified);
    let BlobDiff::Text { hunks } = repo
        .diff_worktree_file(
            "alpha.txt",
            modified.oid,
            DEFAULT_BLOB_LIMIT,
            DEFAULT_DIFF_CONTEXT,
        )
        .unwrap()
    else {
        panic!("expected a text diff");
    };
    assert_eq!(hunks.len(), 1);
    assert!(
        repo.diff_worktree_file("nope.txt", None, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)
            .is_err()
    );
}

#[test]
fn lists_and_reads_the_working_tree_and_scans_status_and_stashes() {
    let fixture = Fixture::standard();
    fixture.write("alpha.txt", b"stash me\n");
    fixture.git(&["stash", "push", "-q", "-m", "kept"]);
    fixture.write("untracked.txt", b"new\n");
    fixture.remove("Zeta.txt");
    let repo = open(&fixture);
    let tree = repo.resolve_tree("HEAD").unwrap();

    let listing = repo.worktree_entries(Some(tree), "").unwrap();
    assert_eq!(listing.missing[0].name, "Zeta.txt");
    let untracked = listing
        .entries
        .iter()
        .find(|e| e.name == "untracked.txt")
        .unwrap();
    assert_eq!(untracked.status, WorktreeStatus::Untracked);
    let BlobContent::Text { data, .. } = repo
        .worktree_file("untracked.txt", DEFAULT_BLOB_LIMIT)
        .unwrap()
    else {
        panic!("expected text");
    };
    assert_eq!(data, b"new\n");
    assert!(repo.worktree_file("nope.txt", DEFAULT_BLOB_LIMIT).is_err());

    let report = repo
        .worktree_status(Some(tree), DEFAULT_STATUS_SCAN_CAP)
        .unwrap();
    assert!(!report.truncated);
    assert!(repo.worktree_status(Some(tree), 1).unwrap().truncated);
    let files = repo
        .worktree_files(Some(tree), DEFAULT_STATUS_SCAN_CAP)
        .unwrap();
    assert!(files.files.iter().any(|file| file.path == "src/main.rs"));
    assert!(repo.worktree_files(Some(tree), 1).unwrap().truncated);

    let stashes = repo.stash_list().unwrap();
    assert_eq!(stashes.len(), 1);
    assert!(stashes[0].message.ends_with("kept"));
    assert_eq!(stashes[0].oid, rev(&fixture, "stash@{0}"));
}

#[test]
fn scans_an_unborn_branch_with_no_tree() {
    let fixture = Fixture::empty_dir();
    fixture.git(&["init", "-q", "-b", "main"]);
    fixture.write("a.txt", b"hi\n");
    let report = open(&fixture)
        .worktree_status(None, DEFAULT_STATUS_SCAN_CAP)
        .unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].status, WorktreeStatus::Untracked);
}

#[test]
fn searches_blobs_and_working_tree_files() {
    let fixture = Fixture::standard();
    fixture.write("notes.txt", b"one\n  Needle here\n");
    let repo = open(&fixture);
    let blobs = ["HEAD:src/main.rs", "HEAD:bin.dat", "HEAD:README.md"]
        .map(|spec| repo.resolve(spec).unwrap());
    let results = repo
        .search_blobs(&blobs, &query("fn"), SearchLimits::default())
        .unwrap();
    assert_eq!(results.len(), 3);
    assert!(matches!(&results[0], FileSearch::Searched { lines, .. } if lines.len() == 1));
    assert_eq!(results[1], FileSearch::Binary);

    let paths = ["notes.txt".to_owned(), "gone.txt".to_owned()];
    let found = repo
        .search_worktree_files(&paths, &query("needle"), SearchLimits::default())
        .unwrap();
    let FileSearch::Searched { lines, .. } = &found[0] else {
        panic!("expected a match, got {:?}", found[0]);
    };
    assert_eq!((lines[0].line, lines[0].column), (1, 2));
    let FileSearch::Unreadable { error } = &found[1] else {
        panic!("expected unreadable, got {:?}", found[1]);
    };
    assert!(error.contains("gone.txt"), "{error}");
}

#[test]
fn the_repository_is_usable_from_many_threads_at_once() {
    let fixture = Fixture::standard();
    let repo: Arc<dyn RepositoryReading> = Arc::new(open(&fixture));
    let root = repo.resolve_tree("main").unwrap();
    let expected = repo.tree_entries(root).unwrap();
    let workers: Vec<_> = (0..16)
        .map(|_| {
            let repo = Arc::clone(&repo);
            thread::spawn(move || repo.tree_entries(root).unwrap())
        })
        .collect();
    for worker in workers {
        assert_eq!(worker.join().unwrap(), expected);
    }
}
