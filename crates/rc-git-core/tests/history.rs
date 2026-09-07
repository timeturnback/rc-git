//! Path-filtered history against a purpose-built branch-and-merge fixture.

use rc_git_core::{CommitPage, Repository};
use rc_git_testkit::Fixture;

/// Six commits, oldest first: `root` adds `file.txt` and `dir/a.txt`; `topic change` (branch
/// `topic`) rewrites `file.txt`; `main change` rewrites `dir/a.txt`; `clean merge` merges
/// `topic` without touching anything itself; `hack change` (branch `hack`, from `root`) adds
/// `other.txt`; `evil merge` merges `hack` and also rewrites `file.txt` in the merge itself.
fn merge_fixture() -> Fixture {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("file.txt", b"base\n");
    fx.write("dir/a.txt", b"a\n");
    fx.commit("root", "2024-01-01T00:00:00+0000");

    fx.git(&["checkout", "-q", "-b", "topic"]);
    fx.write("file.txt", b"topic\n");
    fx.commit("topic change", "2024-01-02T00:00:00+0000");

    fx.git(&["checkout", "-q", "main"]);
    fx.write("dir/a.txt", b"a2\n");
    fx.commit("main change", "2024-01-03T00:00:00+0000");

    fx.git_with_env(
        &["merge", "-q", "--no-ff", "-m", "clean merge", "topic"],
        &[
            ("GIT_AUTHOR_DATE", "2024-01-04T00:00:00+0000"),
            ("GIT_COMMITTER_DATE", "2024-01-04T00:00:00+0000"),
        ],
    );

    fx.git(&["checkout", "-q", "-b", "hack", "main~2"]);
    fx.write("other.txt", b"other\n");
    fx.commit("hack change", "2024-01-05T00:00:00+0000");

    fx.git(&["checkout", "-q", "main"]);
    fx.git_with_env(&["merge", "-q", "--no-ff", "--no-commit", "hack"], &[]);
    fx.write("file.txt", b"evil\n");
    fx.git(&["add", "-A"]);
    fx.git_with_env(
        &[
            "commit",
            "-q",
            "-m",
            "evil merge",
            "--date",
            "2024-01-06T00:00:00+0000",
        ],
        &[("GIT_COMMITTER_DATE", "2024-01-06T00:00:00+0000")],
    );
    fx
}

fn titles(page: &CommitPage) -> Vec<String> {
    page.commits.iter().map(|c| c.title.clone()).collect()
}

#[test]
fn path_history_follows_a_file_through_merges() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let page = repo.path_history(tip, "file.txt", 0, 100, 1000).unwrap();
    assert_eq!(
        titles(&page),
        ["evil merge", "topic change", "root"],
        "the evil merge shows, the clean merge stays hidden, the branch commit is found"
    );
    assert_eq!(page.scanned, 6, "every commit was examined");
    assert!(page.complete);

    let added = repo.path_history(tip, "other.txt", 0, 100, 1000).unwrap();
    assert_eq!(
        titles(&added),
        ["hack change"],
        "the merge bringing it in matches one parent, so only the addition shows"
    );
}

#[test]
fn path_history_covers_folders_roots_and_missing_paths() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let folder = repo.path_history(tip, "dir", 0, 100, 1000).unwrap();
    assert_eq!(titles(&folder), ["main change", "root"]);

    let everything = repo.path_history(tip, "", 0, 100, 1000).unwrap();
    assert_eq!(
        everything.commits.len(),
        6,
        "an empty path names the root tree, which a merge combining both sides always changes"
    );

    let missing = repo.path_history(tip, "nope.txt", 0, 100, 1000).unwrap();
    assert!(missing.commits.is_empty());
    assert!(missing.complete);
    assert_eq!(missing.scanned, 6);
}

#[test]
fn path_history_pages_with_a_scan_cursor() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let whole = repo.path_history(tip, "file.txt", 0, 100, 1000).unwrap();

    let mut collected = Vec::new();
    let mut scan_skip = 0;
    loop {
        let page = repo
            .path_history(tip, "file.txt", scan_skip, 100, 2)
            .unwrap();
        assert!(page.scanned <= 2);
        collected.extend(page.commits);
        scan_skip += page.scanned;
        if page.complete {
            break;
        }
    }
    assert_eq!(collected, whole.commits, "chunked calls equal one big call");

    let capped = repo.path_history(tip, "file.txt", 0, 100, 1).unwrap();
    assert_eq!(titles(&capped), ["evil merge"]);
    assert_eq!(capped.scanned, 1);
    assert!(!capped.complete);

    let limited = repo.path_history(tip, "file.txt", 0, 1, 1000).unwrap();
    assert_eq!(titles(&limited), ["evil merge"]);
    assert!(!limited.complete, "the limit stopped the walk early");
}

#[test]
fn path_history_rejects_a_non_commit_tip() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("main").unwrap();
    assert!(repo.path_history(tree, "file.txt", 0, 100, 1000).is_err());
}
