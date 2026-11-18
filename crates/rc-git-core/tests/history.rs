//! Path-filtered history against a purpose-built branch-and-merge fixture.

use std::sync::OnceLock;

use rc_git_core::{CommitFilter, CommitPage, Error, MergeBase, ObjectKind, Oid, Repository};
use rc_git_testkit::Fixture;

/// How much of a walk one page takes: at most `limit` matches from `max_scan` commits.
#[derive(Clone, Copy)]
struct Window {
    limit: usize,
    max_scan: usize,
}

/// A page wide enough for every fixture here: 100 matches, 1000 commits examined.
const WIDE: Window = window(100, 1000);

const fn window(limit: usize, max_scan: usize) -> Window {
    Window { limit, max_scan }
}

/// The first page of a fresh walk, the shape most tests here read.
trait FirstPage {
    fn path_history(&self, tip: Oid, path: &str, window: Window)
    -> rc_git_core::Result<CommitPage>;
    fn search_commits(
        &self,
        tip: Oid,
        filter: &CommitFilter,
        window: Window,
    ) -> rc_git_core::Result<CommitPage>;
}

impl FirstPage for Repository {
    fn path_history(
        &self,
        tip: Oid,
        path: &str,
        window: Window,
    ) -> rc_git_core::Result<CommitPage> {
        let filter = CommitFilter {
            path: Some(path.to_owned()),
            ..CommitFilter::default()
        };
        self.search_commits(tip, &filter, window)
    }

    fn search_commits(
        &self,
        tip: Oid,
        filter: &CommitFilter,
        window: Window,
    ) -> rc_git_core::Result<CommitPage> {
        self.history_walk(tip, filter.clone())?
            .next_page(self, window.limit, window.max_scan)
    }
}

/// Six commits, oldest first: `root` adds `file.txt` and `dir/a.txt`; `topic change` (branch
/// `topic`) rewrites `file.txt`; `main change` rewrites `dir/a.txt`; `clean merge` merges
/// `topic` without touching anything itself; `hack change` (branch `hack`, from `root`) adds
/// `other.txt`; `evil merge` merges `hack` and also rewrites `file.txt` in the merge itself.
/// Built once per run; each test gets its own copy.
fn merge_fixture() -> Fixture {
    static TEMPLATE: OnceLock<Fixture> = OnceLock::new();
    Fixture::clone_of(TEMPLATE.get_or_init(build_merge_fixture))
}

fn build_merge_fixture() -> Fixture {
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
    fx.git(&["merge", "-q", "--no-ff", "--no-commit", "hack"]);
    fx.write("file.txt", b"evil\n");
    fx.commit("evil merge", "2024-01-06T00:00:00+0000");
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

    let page = repo.path_history(tip, "file.txt", WIDE).unwrap();
    assert_eq!(
        titles(&page),
        ["evil merge", "topic change", "root"],
        "the evil merge shows, the clean merge stays hidden, the branch commit is found"
    );
    assert_eq!(page.scanned, 6, "every commit was examined");
    assert!(page.complete);

    let added = repo.path_history(tip, "other.txt", WIDE).unwrap();
    assert_eq!(
        titles(&added),
        ["hack change"],
        "the merge bringing it in matches one parent, so only the addition shows"
    );
}

#[test]
fn path_history_reads_trees_through_a_commit_graph() {
    let fx = merge_fixture();
    let paths = ["file.txt", "dir/a.txt", "other.txt", "dir", ""];
    let history = |fx: &Fixture| -> Vec<Vec<String>> {
        let repo = Repository::open(fx.path()).unwrap();
        let tip = repo.resolve("main").unwrap();
        paths
            .iter()
            .map(|path| titles(&repo.path_history(tip, path, WIDE).unwrap()))
            .collect()
    };
    let without = history(&fx);

    fx.git(&["commit-graph", "write", "--reachable"]);
    assert!(fx.git_dir().join("objects/info/commit-graph").exists());
    assert_eq!(history(&fx), without, "the graph changes no answer");

    fx.write("file.txt", b"after the graph\n");
    fx.commit("after graph", "2024-01-07T00:00:00+0000");
    let newer = history(&fx);
    assert_eq!(
        newer[0],
        ["after graph", "evil merge", "topic change", "root"],
        "a commit newer than the graph is read from the object database"
    );
    assert_eq!(
        newer[1..4],
        without[1..4],
        "paths the new commit left alone"
    );
}

#[test]
fn path_history_shows_a_mode_only_change() {
    let fx = merge_fixture();
    fx.git(&["update-index", "--chmod=+x", "file.txt"]);
    let date = "2024-01-07T00:00:00+0000";
    fx.git_with_env(
        &["commit", "-q", "-m", "mode change"],
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
    let mode = fx.git_output(&["ls-tree", "main", "file.txt"]);
    assert!(mode.starts_with("100755 "), "only the mode changed: {mode}");
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let page = repo.path_history(tip, "file.txt", WIDE).unwrap();
    let expected = fx.git_output(&["log", "--format=%s", "--", "file.txt"]);
    assert_eq!(titles(&page), expected.lines().collect::<Vec<_>>());
    assert_eq!(
        titles(&page).first().map(String::as_str),
        Some("mode change"),
        "flipping the executable bit touches the path"
    );
}

#[test]
fn path_history_covers_folders_roots_and_missing_paths() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let folder = repo.path_history(tip, "dir", WIDE).unwrap();
    assert_eq!(titles(&folder), ["main change", "root"]);

    let everything = repo.path_history(tip, "", WIDE).unwrap();
    assert_eq!(
        everything.commits.len(),
        6,
        "an empty path names the root tree, which a merge combining both sides always changes"
    );

    let file = repo.path_history(tip, "file.txt", WIDE).unwrap();
    assert!(!file.commits.is_empty());
    assert_eq!(
        repo.path_history(tip, "./file.txt", WIDE).unwrap(),
        file,
        "a './' prefix names the same path"
    );

    let missing = repo.path_history(tip, "nope.txt", WIDE).unwrap();
    assert_eq!(missing.commits.len(), 0);
    assert!(missing.complete);
    assert_eq!(missing.scanned, 6);
}

#[test]
fn path_history_pages_with_a_scan_cursor() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let whole = repo.path_history(tip, "file.txt", WIDE).unwrap();

    let filter = CommitFilter {
        path: Some("file.txt".to_owned()),
        ..CommitFilter::default()
    };
    let walk = repo.history_walk(tip, filter).unwrap();
    let mut collected = Vec::new();
    let mut scanned = 0;
    loop {
        let page = walk.next_page(&repo, 100, 2).unwrap();
        assert!(page.scanned <= 2);
        assert!(
            page.scanned > 0 || page.complete,
            "every incomplete page advances the walk"
        );
        collected.extend(page.commits);
        scanned += page.scanned;
        if page.complete {
            break;
        }
    }
    assert_eq!(collected, whole.commits, "chunked pages equal one big page");
    assert_eq!(scanned, whole.scanned, "no commit is examined twice");
    let after_end = walk.next_page(&repo, 100, 2).unwrap();
    assert_eq!(after_end.scanned, 0);
    assert!(after_end.complete && after_end.commits.is_empty());

    let capped = repo.path_history(tip, "file.txt", window(100, 1)).unwrap();
    assert_eq!(titles(&capped), ["evil merge"]);
    assert_eq!(capped.scanned, 1);
    assert!(!capped.complete);

    let limited = repo.path_history(tip, "file.txt", window(1, 1000)).unwrap();
    assert_eq!(titles(&limited), ["evil merge"]);
    assert!(!limited.complete, "the limit stopped the walk early");
}

#[test]
fn a_zero_scan_cap_still_makes_progress() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let history = repo.path_history(tip, "file.txt", window(100, 0)).unwrap();
    assert_eq!(history.scanned, 1, "a zero cap examines one commit");
    assert_eq!(titles(&history), ["evil merge"]);
    assert!(!history.complete);

    let search = repo
        .search_commits(tip, &CommitFilter::default(), window(100, 0))
        .unwrap();
    assert_eq!(search.scanned, 1, "a zero cap examines one commit");
    assert_eq!(titles(&search), ["evil merge"]);
    assert!(!search.complete);
}

#[test]
fn search_matches_message_title_and_body_case_insensitively() {
    let fx = merge_fixture();
    fx.commit_with_body(
        "body probe",
        "A Needle in the body.",
        "2024-01-07T00:00:00+0000",
    );
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let by_title = CommitFilter {
        message: Some("MERGE".to_owned()),
        ..CommitFilter::default()
    };
    let page = repo.search_commits(tip, &by_title, WIDE).unwrap();
    assert_eq!(titles(&page), ["evil merge", "clean merge"]);
    assert!(page.complete);

    let by_body = CommitFilter {
        message: Some("needle".to_owned()),
        ..CommitFilter::default()
    };
    let page = repo.search_commits(tip, &by_body, WIDE).unwrap();
    assert_eq!(titles(&page), ["body probe"]);
}

#[test]
fn search_under_a_path_keeps_only_commits_that_changed_it() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let search = |message: Option<&str>, path: &str| {
        let filter = CommitFilter {
            message: message.map(str::to_owned),
            path: Some(path.to_owned()),
            ..CommitFilter::default()
        };
        repo.search_commits(tip, &filter, WIDE).unwrap()
    };

    let page = search(Some("change"), "file.txt");
    assert_eq!(
        titles(&page),
        ["topic change"],
        "main and hack changes miss file.txt"
    );
    let page = search(Some("merge"), "file.txt");
    assert_eq!(titles(&page), ["evil merge"], "a clean merge stays hidden");
    assert!(page.complete);
    let page = search(Some("change"), "./dir/a.txt");
    assert_eq!(titles(&page), ["main change"]);

    let path_only = search(None, "file.txt");
    let walked = repo.path_history(tip, "file.txt", WIDE).unwrap();
    assert_eq!(
        titles(&path_only),
        titles(&walked),
        "the same test as path_history"
    );
    assert_eq!(path_only.scanned, walked.scanned);
}

#[test]
fn search_matches_author_name_and_email() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let by_email = CommitFilter {
        author: Some("author@example.com".to_owned()),
        ..CommitFilter::default()
    };
    let page = repo.search_commits(tip, &by_email, WIDE).unwrap();
    assert_eq!(
        page.commits.len(),
        6,
        "every fixture commit shares the author"
    );

    let nobody = CommitFilter {
        author: Some("nobody".to_owned()),
        ..CommitFilter::default()
    };
    let page = repo.search_commits(tip, &nobody, WIDE).unwrap();
    assert_eq!(page.commits.len(), 0);
    assert!(page.complete);

    let empty = CommitFilter::default();
    let page = repo.search_commits(tip, &empty, WIDE).unwrap();
    assert_eq!(page.commits.len(), 6, "an empty filter matches everything");

    let by_name = CommitFilter {
        author: Some("test AUTHOR".to_owned()),
        ..CommitFilter::default()
    };
    let page = repo.search_commits(tip, &by_name, WIDE).unwrap();
    assert_eq!(page.commits.len(), 6, "the name matches, ASCII case folded");

    for committer in ["Test Committer", "committer@example.com"] {
        let filter = CommitFilter {
            author: Some(committer.to_owned()),
            ..CommitFilter::default()
        };
        let page = repo.search_commits(tip, &filter, WIDE).unwrap();
        assert_eq!(
            page.commits.len(),
            0,
            "{committer}: committer identity is not searched"
        );
        assert!(page.complete);
    }
}

#[test]
fn search_requires_every_set_field_to_match() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let search = |filter: CommitFilter| repo.search_commits(tip, &filter, WIDE).unwrap();

    let page = search(CommitFilter {
        message: Some("merge".to_owned()),
        before: Some(1_704_326_400),
        ..CommitFilter::default()
    });
    assert_eq!(
        titles(&page),
        ["clean merge"],
        "the evil merge is newer than the ceiling"
    );

    let page = search(CommitFilter {
        message: Some("change".to_owned()),
        author: Some("nobody".to_owned()),
        ..CommitFilter::default()
    });
    assert_eq!(
        titles(&page),
        Vec::<String>::new(),
        "an OR would return the change commits"
    );

    let page = search(CommitFilter {
        message: Some("merge".to_owned()),
        author: Some("nobody".to_owned()),
        ..CommitFilter::default()
    });
    assert_eq!(page.commits.len(), 0);
    assert!(page.complete);

    let page = search(CommitFilter {
        message: Some("merge".to_owned()),
        author: Some("author@example.com".to_owned()),
        ..CommitFilter::default()
    });
    assert_eq!(titles(&page), ["evil merge", "clean merge"]);

    let page = search(CommitFilter {
        message: Some("change".to_owned()),
        after: Some(1_704_240_000),
        before: Some(1_704_412_800),
        ..CommitFilter::default()
    });
    assert_eq!(
        titles(&page),
        ["hack change", "main change"],
        "newest first; the floor is inclusive"
    );
}

#[test]
fn search_time_floor_terminates_the_walk_and_ceiling_filters() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let floor = CommitFilter {
        after: Some(1_704_326_400),
        ..CommitFilter::default()
    };
    let page = repo.search_commits(tip, &floor, WIDE).unwrap();
    assert_eq!(titles(&page), ["evil merge", "hack change", "clean merge"]);
    assert_eq!(
        page.scanned, 3,
        "the cutoff stopped the walk before older commits"
    );
    assert!(page.complete);

    let ceiling = CommitFilter {
        before: Some(1_704_153_600),
        ..CommitFilter::default()
    };
    let page = repo.search_commits(tip, &ceiling, WIDE).unwrap();
    assert_eq!(titles(&page), ["topic change", "root"]);
    assert_eq!(
        page.scanned, 6,
        "a ceiling filters but cannot stop the walk"
    );
}

/// Adds an empty commit titled `title` on `main` with separate author and committer dates.
fn commit_with_dates(fx: &Fixture, title: &str, author_date: &str, committer_date: &str) {
    fx.git_with_env(
        &["commit", "-q", "--allow-empty", "-m", title],
        &[
            ("GIT_AUTHOR_DATE", author_date),
            ("GIT_COMMITTER_DATE", committer_date),
        ],
    );
}

#[test]
fn commit_details_keeps_author_and_committer_offsets_apart() {
    let fx = merge_fixture();
    commit_with_dates(
        &fx,
        "zoned",
        "2024-01-04T00:00:00-0130",
        "2024-01-04T00:00:00+0545",
    );
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let detail = repo.commit_details(tip).unwrap();
    assert_eq!(detail.summary.author_offset, -5_400);
    assert_eq!(
        detail.summary.author_time, 1_704_331_800,
        "2024-01-04T01:30:00Z"
    );
    assert_eq!(detail.committer_offset, 20_700);
    assert_eq!(detail.committer_time, 1_704_305_700, "2024-01-03T18:15:00Z");
}

#[test]
fn search_time_filter_reads_committer_time() {
    let fx = merge_fixture();
    commit_with_dates(
        &fx,
        "skewed",
        "2020-01-01T00:00:00-0130",
        "2024-01-07T00:00:00+0545",
    );
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let floor = CommitFilter {
        after: Some(1_704_564_900),
        ..CommitFilter::default()
    };
    let page = repo.search_commits(tip, &floor, WIDE).unwrap();
    assert_eq!(titles(&page), ["skewed"], "committed 2024-01-06T18:15:00Z");
    let skewed = page.commits.first().unwrap();
    assert_eq!(
        skewed.author_time, 1_577_842_200,
        "authored 2020-01-01T01:30:00Z, far below the floor"
    );

    let ceiling = CommitFilter {
        before: Some(1_704_499_200),
        ..CommitFilter::default()
    };
    let page = repo.search_commits(tip, &ceiling, WIDE).unwrap();
    assert_eq!(
        page.commits.len(),
        6,
        "authored in 2020, still above the ceiling"
    );
    assert!(!titles(&page).contains(&"skewed".to_owned()));
}

#[test]
fn search_pages_with_a_scan_cursor_even_when_nothing_matches() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let nothing = CommitFilter {
        message: Some("zzz".to_owned()),
        ..CommitFilter::default()
    };

    let walk = repo.history_walk(tip, nothing).unwrap();
    let page = walk.next_page(&repo, 100, 2).unwrap();
    assert_eq!(page.commits.len(), 0);
    assert_eq!(page.scanned, 2);
    assert!(
        !page.complete,
        "the cap stopped the walk, not the end of history"
    );

    let rest = walk.next_page(&repo, 100, 1000).unwrap();
    assert_eq!(rest.commits.len(), 0);
    assert_eq!(rest.scanned, 4);
    assert!(rest.complete);
}

#[test]
fn path_history_rejects_a_non_commit_tip() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("main").unwrap();
    let err = repo.path_history(tree, "file.txt", WIDE).unwrap_err();
    assert!(
        matches!(
            err,
            Error::WrongKind {
                expected: ObjectKind::Commit,
                actual: ObjectKind::Tree,
                ..
            }
        ),
        "{err}"
    );
}

/// A `--depth 2` clone of `origin` in a sibling temp dir.
fn shallow_clone(origin: &Fixture) -> Fixture {
    let clone = Fixture::empty_dir();
    let url = format!("file://{}", origin.path().display());
    clone.git(&["clone", "-q", "--depth", "2", &url, "."]);
    assert!(
        clone.git_dir().join("shallow").is_file(),
        "the clone is shallow"
    );
    clone
}

#[test]
fn path_history_treats_the_shallow_boundary_as_a_root() {
    let fx = merge_fixture();
    let clone = shallow_clone(&fx);
    let repo = Repository::open(clone.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let page = repo.path_history(tip, "file.txt", WIDE).unwrap();
    let oids: Vec<String> = page.commits.iter().map(|c| c.oid.to_string()).collect();
    let expected = clone.git_output(&["log", "--format=%H", "--", "file.txt"]);
    assert_eq!(oids, expected.lines().collect::<Vec<_>>());
    let expected = clone.git_output(&["log", "--format=%s", "--", "file.txt"]);
    assert_eq!(titles(&page), expected.lines().collect::<Vec<_>>());
    assert!(
        titles(&page).contains(&"clean merge".to_owned()),
        "a boundary merge holding the file matches like a root"
    );
    assert!(page.complete);
}

#[test]
fn path_history_still_fails_on_a_missing_parent_outside_a_shallow_clone() {
    let fx = merge_fixture();
    let root = fx.git_output(&["rev-list", "--max-parents=0", "main"]);
    fx.remove(&format!(".git/objects/{}/{}", &root[..2], &root[2..]));
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let err = repo.path_history(tip, "file.txt", WIDE).unwrap_err();
    assert!(
        matches!(&err, Error::Object { op: "read commit", oid, .. } if oid.to_string() == root),
        "{err:?}"
    );
}

#[test]
fn path_history_reports_an_unreadable_tree() {
    let fx = merge_fixture();
    let root_tree = fx.rev_parse("main~3^{tree}");
    fx.remove(&format!(
        ".git/objects/{}/{}",
        &root_tree[..2],
        &root_tree[2..]
    ));
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let err = repo.path_history(tip, "file.txt", WIDE).unwrap_err();
    assert!(
        matches!(&err, Error::Object { op: "read tree", oid, .. }
            if oid.to_string() == root_tree),
        "{err:?}"
    );
    assert!(err.to_string().contains(&root_tree), "{err}");
}

#[test]
fn search_rejects_a_non_commit_tip() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("main").unwrap();
    let err = repo
        .search_commits(tree, &CommitFilter::default(), WIDE)
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::WrongKind {
                op: "read commit",
                expected: ObjectKind::Commit,
                actual: ObjectKind::Tree,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn path_history_reads_a_path_under_a_submodule_as_absent() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("lib/x.c", b"x\n");
    fx.commit("add lib", "2024-01-01T00:00:00+0000");
    fx.git(&["rm", "-r", "-q", "lib"]);
    let gitlink = "160000,1111111111111111111111111111111111111111,lib";
    fx.git(&["update-index", "--add", "--cacheinfo", gitlink]);
    let date = "2024-01-02T00:00:00+0000";
    fx.git_with_env(
        &["commit", "-q", "-m", "lib becomes a submodule"],
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let page = repo.path_history(tip, "lib/x.c", WIDE).unwrap();
    let expected = fx.git_output(&["log", "--format=%s", "--", "lib/x.c"]);
    assert_eq!(titles(&page), expected.lines().collect::<Vec<_>>());
    assert_eq!(titles(&page), ["lib becomes a submodule", "add lib"]);

    let root = repo.resolve_tree("main").unwrap();
    assert_eq!(
        repo.entry_at_path(root, "lib/x.c").unwrap(),
        None,
        "a path through a gitlink is absent, not an unreadable object"
    );
}

// --- merge base ---

/// `git merge-base --all <one> <two>`, one id per line.
fn git_merge_bases(fx: &Fixture, one: &str, two: &str) -> Vec<String> {
    fx.git_output(&["merge-base", "--all", one, two])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The base found, `None` for any other answer.
fn found(base: &MergeBase) -> Option<String> {
    match base {
        MergeBase::Found { base, .. } => Some(base.to_string()),
        MergeBase::Unrelated { .. } | MergeBase::Incomplete { .. } => None,
    }
}

/// Runs `git <args>` with the author and committer dates set to `date`.
fn git_dated(fx: &Fixture, args: &[&str], date: &str) {
    fx.git_with_env(
        args,
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
}

#[test]
fn merge_base_finds_where_a_branch_left_its_base_as_git_does() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    for (one, two) in [("main", "hack"), ("hack", "main"), ("main~1", "topic")] {
        let (a, b) = (repo.resolve(one).unwrap(), repo.resolve(two).unwrap());
        let base = repo.merge_base(a, b, 1000).unwrap();
        assert_eq!(
            found(&base).into_iter().collect::<Vec<_>>(),
            git_merge_bases(&fx, one, two),
            "{one}...{two}"
        );
    }
}

#[test]
fn merge_base_of_an_ancestor_is_the_ancestor_and_of_itself_is_itself() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let topic = repo.resolve("topic").unwrap();
    let base = repo.merge_base(tip, topic, 1000).unwrap();
    assert_eq!(found(&base), Some(fx.rev_parse("topic")));
    assert_eq!(
        repo.merge_base(tip, tip, 0).unwrap(),
        MergeBase::Found {
            base: tip,
            scanned: 0
        }
    );
}

#[test]
fn merge_base_picks_one_best_base_of_a_criss_cross() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("f", b"0\n");
    fx.commit("root", "2024-01-01T00:00:00+0000");
    fx.git(&["checkout", "-q", "-b", "side"]);
    fx.write("s", b"1\n");
    fx.commit("side one", "2024-01-02T00:00:00+0000");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("m", b"1\n");
    fx.commit("main one", "2024-01-03T00:00:00+0000");
    let take_side = ["merge", "-q", "--no-ff", "-m", "main takes side", "side"];
    git_dated(&fx, &take_side, "2024-01-04T00:00:00+0000");
    fx.git(&["checkout", "-q", "side"]);
    let take_main = ["merge", "-q", "--no-ff", "-m", "side takes main", "main~1"];
    git_dated(&fx, &take_main, "2024-01-05T00:00:00+0000");
    let bases = git_merge_bases(&fx, "main", "side");
    assert_eq!(
        bases.len(),
        2,
        "a criss-cross has two best bases: {bases:?}"
    );

    let repo = Repository::open(fx.path()).unwrap();
    let (main, side) = (repo.resolve("main").unwrap(), repo.resolve("side").unwrap());
    let base = found(&repo.merge_base(main, side, 1000).unwrap()).unwrap();
    assert!(bases.contains(&base), "{base} is not among {bases:?}");
    assert_eq!(base, fx.rev_parse("main~1"), "the newer of the two");
}

#[test]
fn merge_base_of_unrelated_histories_says_so() {
    let fx = merge_fixture();
    fx.git(&["checkout", "-q", "--orphan", "island"]);
    fx.write("island.txt", b"alone\n");
    fx.commit("island", "2024-02-01T00:00:00+0000");
    let repo = Repository::open(fx.path()).unwrap();
    let main = repo.resolve("main").unwrap();
    let island = repo.resolve("island").unwrap();
    let all = fx.git_output(&["rev-list", "--count", "main", "island"]);
    assert_eq!(
        repo.merge_base(main, island, 1000).unwrap(),
        MergeBase::Unrelated {
            scanned: all.parse().unwrap()
        }
    );
}

#[test]
fn merge_base_stops_at_max_scan_without_guessing() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let main = repo.resolve("main~1").unwrap();
    let hack = repo.resolve("hack").unwrap();
    assert_eq!(
        repo.merge_base(main, hack, 2).unwrap(),
        MergeBase::Incomplete { scanned: 2 }
    );
    assert!(matches!(
        repo.merge_base(main, hack, 1000).unwrap(),
        MergeBase::Found { .. }
    ));
}

#[test]
fn merge_base_is_incomplete_at_a_shallow_edge_it_needed() {
    let fx = merge_fixture();
    fx.git(&["checkout", "-q", "-b", "early", "main~3"]);
    fx.write("early.txt", b"early\n");
    fx.commit("early change", "2024-01-07T00:00:00+0000");
    fx.git(&["checkout", "-q", "main"]);
    let clone = Fixture::empty_dir();
    let url = format!("file://{}", fx.path().display());
    clone.git(&[
        "clone",
        "-q",
        "--depth",
        "1",
        "--no-single-branch",
        &url,
        ".",
    ]);
    let repo = Repository::open(clone.path()).unwrap();
    let main = repo.resolve("origin/main").unwrap();
    let early = repo.resolve("origin/early").unwrap();
    assert!(
        matches!(
            repo.merge_base(main, early, 1000).unwrap(),
            MergeBase::Incomplete { .. }
        ),
        "the base lies past the shallow edge"
    );
}

#[test]
fn merge_base_reports_a_missing_commit_by_id() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();
    let missing = Oid::from_hex(&"ab".repeat(20)).unwrap();
    let err = repo.merge_base(tip, missing, 1000).unwrap_err();
    assert!(
        matches!(&err, Error::Object { oid, .. } if *oid == missing),
        "{err:?}"
    );
    let tree = repo.resolve_tree("main").unwrap();
    let err = repo.merge_base(tip, tree, 1000).unwrap_err();
    assert!(
        matches!(
            err,
            Error::WrongKind {
                actual: ObjectKind::Tree,
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn has_object_answers_for_present_and_missing_ids() {
    let fx = merge_fixture();
    let repo = Repository::open(fx.path()).unwrap();
    assert!(repo.has_object(repo.resolve("main").unwrap()));
    assert!(repo.has_object(repo.resolve_tree("main").unwrap()));
    assert!(!repo.has_object(Oid::from_hex(&"ab".repeat(20)).unwrap()));
}
