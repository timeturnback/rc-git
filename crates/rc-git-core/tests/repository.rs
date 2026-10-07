//! Every public operation, exercised against the standard fixture.
#![expect(clippy::unwrap_used, reason = "test helpers panic on fixture failure")]

use std::sync::Arc;

use rc_git_core::{
    BlobContent, DEFAULT_BLOB_LIMIT, EntryKind, Error, ObjectKind, Oid, RefKind, Repository,
    WorktreeStatus,
};
use rc_git_testkit::{
    BIN_DAT, COMMIT_1_TITLE, COMMIT_2_TITLE, FEATURE_TITLE, Fixture, MAIN_RS, README,
};

fn oid(hex: &str) -> Oid {
    Oid::from_hex(hex).unwrap()
}

fn names(entries: &[rc_git_core::TreeEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| e.name_lossy().into_owned())
        .collect()
}

#[test]
fn opens_from_work_dir_and_from_git_dir() {
    let fx = Fixture::standard();
    let from_work = Repository::open(fx.path()).unwrap();
    assert!(from_work.git_dir().ends_with(".git"));
    assert!(from_work.work_dir().is_some());

    let from_git = Repository::open(fx.git_dir()).unwrap();
    assert_eq!(from_git.head().unwrap(), from_work.head().unwrap());
    assert!(format!("{from_work:?}").contains("pool_slots"));
}

#[test]
fn open_names_the_path_when_not_a_repository() {
    let fx = Fixture::empty_dir();
    let err = Repository::open(fx.path()).unwrap_err();
    assert!(matches!(err, Error::Open { .. }));
    let msg = err.to_string();
    assert!(msg.contains(&fx.path().display().to_string()));
    assert_no_source_location(&msg);
}

fn assert_no_source_location(msg: &str) {
    assert!(!msg.contains(".rs:"), "{msg}");
    assert!(!msg.contains(", at /"), "{msg}");
}

#[test]
fn head_on_a_branch() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let head = repo.head().unwrap();
    assert_eq!(head.ref_name.as_deref(), Some("refs/heads/main"));
    assert_eq!(head.target, Some(oid(&fx.rev_parse("HEAD"))));
}

#[test]
fn head_detached() {
    let fx = Fixture::standard();
    fx.git(&["checkout", "-q", "v0.9"]);
    let repo = Repository::open(fx.path()).unwrap();
    let head = repo.head().unwrap();
    assert_eq!(head.ref_name, None);
    assert_eq!(head.target, Some(oid(&fx.rev_parse("v0.9"))));
}

#[test]
fn head_unborn() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    let repo = Repository::open(fx.path()).unwrap();
    let head = repo.head().unwrap();
    assert_eq!(head.ref_name.as_deref(), Some("refs/heads/main"));
    assert_eq!(head.target, None);
}

#[test]
fn the_default_branch_falls_back_to_a_local_master_or_main() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(
        repo.default_branch().unwrap().as_deref(),
        Some("main"),
        "no remote HEAD, so the local mainline answers"
    );

    fx.git(&["branch", "master", "main"]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(
        repo.default_branch().unwrap().as_deref(),
        Some("master"),
        "master wins over main when both exist locally"
    );
}

#[test]
fn a_remote_head_names_the_default_branch() {
    let fx = Fixture::standard();
    fx.git(&["update-ref", "refs/remotes/origin/trunk", "main"]);
    fx.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(
        repo.default_branch().unwrap().as_deref(),
        Some("trunk"),
        "the remote's HEAD wins over the local fallback, even with no such local branch"
    );
}

#[test]
fn a_repository_with_no_branches_has_no_default_branch() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "work"]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(repo.default_branch().unwrap(), None);
}

#[test]
fn refs_are_classified_and_peeled() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let listing = repo.refs().unwrap();
    assert_eq!(listing.broken.len(), 0);
    assert_eq!(listing.refs.len(), 5);

    let find = |name: &str| listing.refs.iter().find(|r| r.full_name == name).unwrap();
    let main = find("refs/heads/main");
    assert_eq!(
        (main.kind, main.short_name.as_str()),
        (RefKind::LocalBranch, "main")
    );
    assert_eq!(main.target, oid(&fx.rev_parse("main")));

    let feature = find("refs/heads/feature");
    assert_eq!(feature.kind, RefKind::LocalBranch);
    assert_eq!(feature.target, oid(&fx.rev_parse("feature")));

    let remote = find("refs/remotes/origin/main");
    assert_eq!(
        (remote.kind, remote.short_name.as_str()),
        (RefKind::RemoteBranch, "origin/main")
    );

    let light = find("refs/tags/v0.9");
    assert_eq!(
        (light.kind, light.short_name.as_str()),
        (RefKind::Tag, "v0.9")
    );
    assert_eq!(light.target, oid(&fx.rev_parse("v0.9")));

    let annotated = find("refs/tags/v1.0");
    assert_eq!(annotated.kind, RefKind::Tag);
    assert_eq!(
        annotated.target,
        oid(&fx.rev_parse("v1.0^{commit}")),
        "peeled through the tag"
    );
    assert_ne!(
        annotated.target,
        oid(&fx.rev_parse("v1.0")),
        "not the tag object itself"
    );
}

#[test]
fn a_broken_ref_is_reported_without_hiding_the_rest() {
    let fx = Fixture::standard();
    fx.write(
        ".git/refs/heads/broken",
        b"0000000000000000000000000000000000000001\n",
    );
    let repo = Repository::open(fx.path()).unwrap();
    let listing = repo.refs().unwrap();
    assert_eq!(listing.broken.len(), 1);
    assert_eq!(listing.broken[0].full_name, "refs/heads/broken");
    assert_ne!(listing.broken[0].error.len(), 0);
    assert!(
        listing
            .refs
            .iter()
            .any(|r| r.full_name == "refs/heads/main")
    );
}

#[test]
fn resolve_accepts_rev_parse_syntax() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    for spec in [
        "main",
        "feature",
        "v1.0",
        "v0.9",
        "HEAD",
        "HEAD~1",
        "origin/main",
        "main^{tree}",
    ] {
        assert_eq!(
            repo.resolve(spec).unwrap(),
            oid(&fx.rev_parse(spec)),
            "{spec}"
        );
    }
    let full = fx.rev_parse("main");
    assert_eq!(
        repo.resolve(&full[..7]).unwrap(),
        oid(&full),
        "abbreviated hash"
    );

    let err = repo.resolve("nope").unwrap_err();
    assert!(matches!(err, Error::RevSpec { .. }));
    let msg = err.to_string();
    assert!(msg.starts_with("'nope' is not a revision"), "{msg}");
    assert!(msg.contains("could not be found"), "{msg}");
    assert!(msg.matches("couldn't parse revision").count() <= 1, "{msg}");
    assert_no_source_location(&msg);
}

#[test]
fn resolve_tree_peels_tags_and_commits() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let expected = oid(&fx.rev_parse("main^{tree}"));
    assert_eq!(repo.resolve_tree("v1.0").unwrap(), expected);
    assert_eq!(repo.resolve_tree("main").unwrap(), expected);
    assert_eq!(repo.resolve_tree("main^{tree}").unwrap(), expected);
    assert_eq!(repo.tree_of(oid(&fx.rev_parse("v1.0"))).unwrap(), expected);

    let blob = oid(&fx.rev_parse("main:README.md"));
    let err = repo.tree_of(blob).unwrap_err();
    assert!(matches!(
        err,
        Error::WrongKind {
            expected: ObjectKind::Tree,
            actual: ObjectKind::Blob,
            ..
        }
    ));
}

#[test]
fn tree_entries_list_one_level_sorted_for_display() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let root = repo.resolve_tree("main").unwrap();
    let entries = repo.tree_entries(root).unwrap();
    assert_eq!(
        names(&entries),
        [
            "docs",
            "src",
            "alpha.txt",
            "bin.dat",
            "link",
            "README.md",
            "Zeta.txt"
        ]
    );
    let kind = |name: &str| entries.iter().find(|e| e.name == name).unwrap().kind;
    assert_eq!(kind("docs"), EntryKind::Tree);
    assert_eq!(kind("link"), EntryKind::Symlink);
    assert_eq!(kind("README.md"), EntryKind::Blob);

    let src = entries.iter().find(|e| e.name == "src").unwrap().oid;
    assert_eq!(
        names(&repo.tree_entries(src).unwrap()),
        ["lib.rs", "main.rs"]
    );
    assert_eq!(src, oid(&fx.rev_parse("main:src")));

    let blob = oid(&fx.rev_parse("main:README.md"));
    assert!(matches!(
        repo.tree_entries(blob).unwrap_err(),
        Error::Object { .. }
    ));
}

#[test]
fn entry_at_path_walks_segments() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let root = repo.resolve_tree("main").unwrap();
    let main_rs = oid(&fx.rev_parse("main:src/main.rs"));

    let hit = repo.entry_at_path(root, "src/main.rs").unwrap().unwrap();
    assert_eq!((hit.oid, hit.kind), (main_rs, EntryKind::Blob));
    assert_eq!(hit.name, "main.rs");
    assert_eq!(
        repo.entry_at_path(root, "/src//main.rs/")
            .unwrap()
            .unwrap()
            .oid,
        main_rs
    );
    assert_eq!(
        repo.entry_at_path(root, "./src/./main.rs")
            .unwrap()
            .unwrap()
            .oid,
        main_rs,
        "a '.' segment names the folder it is in"
    );
    assert_eq!(
        repo.entry_at_path(root, "src").unwrap().unwrap().kind,
        EntryKind::Tree
    );
    assert_eq!(repo.entry_at_path(root, "missing").unwrap(), None);
    assert_eq!(repo.entry_at_path(root, "src/missing").unwrap(), None);
    assert_eq!(repo.entry_at_path(root, "README.md/deeper").unwrap(), None);
}

#[test]
fn read_blob_distinguishes_text_binary_and_oversize() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let main_rs = oid(&fx.rev_parse("main:src/main.rs"));
    let bin = oid(&fx.rev_parse("main:bin.dat"));
    let readme = oid(&fx.rev_parse("main:README.md"));

    assert_eq!(
        repo.read_blob(main_rs, DEFAULT_BLOB_LIMIT).unwrap(),
        BlobContent::Text {
            data: MAIN_RS.to_vec(),
            line_starts: vec![0, 12, 32],
            valid_utf8: true,
        }
    );
    assert_eq!(
        repo.read_blob(bin, DEFAULT_BLOB_LIMIT).unwrap(),
        BlobContent::Binary {
            size: BIN_DAT.len() as u64
        }
    );
    assert_eq!(
        repo.read_blob(readme, 3).unwrap(),
        BlobContent::TooLarge {
            size: README.len() as u64,
            limit: 3
        }
    );
    assert_eq!(repo.blob_size(readme).unwrap(), README.len() as u64);

    let tree = repo.resolve_tree("main").unwrap();
    assert!(matches!(
        repo.read_blob(tree, DEFAULT_BLOB_LIMIT).unwrap_err(),
        Error::WrongKind {
            expected: ObjectKind::Blob,
            actual: ObjectKind::Tree,
            ..
        }
    ));
    assert!(matches!(
        repo.blob_size(tree).unwrap_err(),
        Error::WrongKind { .. }
    ));
}

#[test]
fn log_is_newest_first_and_pages() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let main = repo.resolve("main").unwrap();
    let feature = repo.resolve("feature").unwrap();
    let titles = |commits: &[rc_git_core::CommitSummary]| -> Vec<String> {
        commits.iter().map(|c| c.title.clone()).collect()
    };

    let all = repo.log(main, 0, 10).unwrap();
    assert_eq!(titles(&all), [COMMIT_2_TITLE, COMMIT_1_TITLE]);
    assert_eq!(all[0].parents, [all[1].oid]);
    assert_eq!(all[1].parents.len(), 0);
    assert_eq!(all[1].author_name, "Test Author");
    assert_eq!(all[1].author_email, "author@example.com");
    assert_eq!(all[1].author_time, 1_704_067_200, "2024-01-01T00:00:00Z");
    assert_eq!(all[1].author_offset, 0);

    assert_eq!(
        titles(&repo.log(feature, 0, 10).unwrap()),
        [FEATURE_TITLE, COMMIT_1_TITLE]
    );
    assert_eq!(titles(&repo.log(main, 1, 10).unwrap()), [COMMIT_1_TITLE]);
    assert_eq!(titles(&repo.log(main, 0, 1).unwrap()), [COMMIT_2_TITLE]);
    assert_eq!(repo.log(main, 5, 10).unwrap().len(), 0);

    assert_eq!(repo.commit(all[0].oid).unwrap(), all[0]);
    let tree = repo.resolve_tree("main").unwrap();
    assert!(matches!(
        repo.commit(tree).unwrap_err(),
        Error::Object { .. }
    ));
}

#[test]
fn log_reads_through_a_commit_graph() {
    let fx = Fixture::standard();
    fx.git(&["commit-graph", "write", "--reachable"]);
    assert!(fx.git_dir().join("objects/info/commit-graph").exists());
    let repo = Repository::open(fx.path()).unwrap();
    let main = repo.resolve("main").unwrap();
    let titles: Vec<_> = repo
        .log(main, 0, 10)
        .unwrap()
        .into_iter()
        .map(|c| c.title)
        .collect();
    assert_eq!(titles, [COMMIT_2_TITLE, COMMIT_1_TITLE]);
}

#[test]
fn commit_details_reports_body_committer_and_tree() {
    let fx = Fixture::standard();
    fx.commit_with_body(
        "Detail commit",
        "Body line one.\n\nBody line two.",
        "2024-01-04T00:00:00+0000",
    );
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let detail = repo.commit_details(tip).unwrap();
    assert_eq!(detail.summary, repo.commit(tip).unwrap());
    assert_eq!(detail.summary.title, "Detail commit");
    assert_eq!(detail.body, "Body line one.\n\nBody line two.");
    assert_eq!(detail.committer_name, "Test Committer");
    assert_eq!(detail.committer_email, "committer@example.com");
    assert_eq!(detail.committer_time, 1_704_326_400, "2024-01-04T00:00:00Z");
    assert_eq!(detail.committer_offset, 0);
    assert_eq!(detail.tree, repo.tree_of(tip).unwrap());

    let titled_only = repo.resolve("main~1").unwrap();
    assert_eq!(repo.commit_details(titled_only).unwrap().body.len(), 0);

    let tree = repo.resolve_tree("main").unwrap();
    assert!(matches!(
        repo.commit_details(tree).unwrap_err(),
        Error::Object { .. }
    ));
}

#[test]
fn concurrent_readers_share_one_repository() {
    let fx = Fixture::standard();
    let repo = Arc::new(Repository::open(fx.path()).unwrap());
    let root = repo.resolve_tree("main").unwrap();
    let main_rs = oid(&fx.rev_parse("main:src/main.rs"));
    let expected = repo.tree_entries(root).unwrap();

    let mut handles = Vec::new();
    for _ in 0..8 {
        let repo = Arc::clone(&repo);
        let expected = expected.clone();
        handles.push(std::thread::spawn(move || {
            for _ in 0..50 {
                assert_eq!(repo.tree_entries(root).unwrap(), expected);
                let content = repo.read_blob(main_rs, DEFAULT_BLOB_LIMIT).unwrap();
                assert!(matches!(content, BlobContent::Text { .. }));
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn an_index_shorter_than_its_trailer_is_read_as_unavailable() {
    let fx = Fixture::standard();
    fx.write(".gitignore", b"*.log\nbuild/\n");
    fx.write("newdir/untracked.txt", b"x\n");
    fx.write("newdir/noise.log", b"x\n");
    fx.write("build/out.txt", b"x\n");
    let repo = Repository::open(fx.path()).unwrap();
    let tree = repo.resolve_tree("HEAD").unwrap();
    for len in [0, 12, 31] {
        fx.write(".git/index", &vec![0; len]);

        let report = repo.worktree_status(Some(tree), 10_000).unwrap();
        let paths: Vec<String> = report.entries.iter().map(|e| e.path.to_string()).collect();
        assert_eq!(
            paths,
            [".gitignore", "newdir/untracked.txt"],
            "index of {len} bytes"
        );

        let listing = repo.worktree_entries(Some(tree), "newdir").unwrap();
        let statuses: Vec<(String, WorktreeStatus)> = listing
            .entries
            .iter()
            .map(|e| (e.name.to_string(), e.status))
            .collect();
        assert_eq!(
            statuses,
            [
                ("noise.log".to_owned(), WorktreeStatus::Ignored),
                ("untracked.txt".to_owned(), WorktreeStatus::Untracked),
            ],
            "index of {len} bytes"
        );

        let files = repo.worktree_files(Some(tree), 10_000).unwrap();
        let paths: Vec<String> = files.files.iter().map(|f| f.path.to_string()).collect();
        assert!(
            paths.iter().any(|p| p == "newdir/untracked.txt"),
            "index of {len} bytes: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with("build/")),
            "an ignored directory is still pruned over an empty index: {paths:?}"
        );
    }
}
