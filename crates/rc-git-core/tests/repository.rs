//! Open, refs, resolve, trees, blobs, log, commit details and an unreadable index, against the
//! standard fixture.
#![expect(clippy::unwrap_used, reason = "test helpers panic on fixture failure")]

use std::sync::Arc;

use rc_git_core::bstr::ByteSlice;
use rc_git_core::{
    BlobContent, CommitSummary, DEFAULT_BLOB_LIMIT, EntryKind, Error, ObjectKind, Oid, RefKind,
    Repository, WorktreeStatus,
};
use rc_git_testkit::{
    BIN_DAT, COMMIT_1_TITLE, COMMIT_2_TITLE, FEATURE_TITLE, Fixture, MAIN_RS, README,
};

fn oid(hex: &str) -> Oid {
    Oid::from_hex(hex).unwrap()
}

/// One page of the log from a fresh cursor, the shape most log tests here read.
trait LogPage {
    fn log(&self, tip: Oid, skip: usize, limit: usize) -> rc_git_core::Result<Vec<CommitSummary>>;
}

impl LogPage for Repository {
    fn log(&self, tip: Oid, skip: usize, limit: usize) -> rc_git_core::Result<Vec<CommitSummary>> {
        let cursor = self.log_cursor(tip)?;
        cursor.skip(skip)?;
        cursor.next_page(self, limit)
    }
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
    assert_eq!(from_git.work_dir(), from_work.work_dir());
    assert_eq!(from_git.git_dir(), from_work.git_dir());
    assert!(format!("{from_work:?}").contains(&from_work.git_dir().display().to_string()));
}

#[test]
fn common_dir_of_a_linked_worktree_is_the_shared_git_dir() {
    let fx = Fixture::standard();
    assert_eq!(
        Repository::open(fx.path()).unwrap().common_dir(),
        Repository::open(fx.path()).unwrap().git_dir(),
        "the main worktree's common dir is its git dir"
    );

    let outside = Fixture::empty_dir();
    let linked = outside.path().join("linked");
    fx.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "wt",
        linked.to_str().unwrap(),
    ]);
    let repo = Repository::open(&linked).unwrap();
    let expected = fx.git_output(&[
        "-C",
        linked.to_str().unwrap(),
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ]);
    let canonical = |path: &std::path::Path| std::fs::canonicalize(path).unwrap();
    assert_eq!(
        canonical(repo.common_dir()),
        canonical(std::path::Path::new(expected.trim()))
    );
    assert_ne!(canonical(repo.git_dir()), canonical(repo.common_dir()));
    assert!(format!("{repo:?}").contains("common_dir"));
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

#[test]
fn open_refuses_a_reftable_repository_by_name() {
    let Some(fx) = Fixture::reftable() else {
        return;
    };
    let err = Repository::open(fx.path()).unwrap_err();
    assert!(matches!(err, Error::Open { .. }), "{err}");
    let msg = err.to_string();
    assert!(
        msg.contains("reftable repositories are not supported"),
        "{msg}"
    );
    assert!(msg.contains(&fx.path().display().to_string()), "{msg}");
}

#[test]
fn open_refuses_a_core_worktree_outside_the_repository() {
    let fx = Fixture::standard();
    let outside = Fixture::empty_dir();
    let outside_path = outside.path().to_str().unwrap();
    fx.git(&["config", "core.worktree", outside_path]);
    for opened in [fx.path().to_path_buf(), fx.git_dir()] {
        let err = Repository::open(&opened).unwrap_err();
        assert!(matches!(err, Error::Open { .. }), "{err}");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("core.worktree = {outside_path}")),
            "{msg}"
        );
        assert!(msg.contains(&opened.display().to_string()), "{msg}");
    }

    fx.git(&["config", "core.worktree", fx.path().to_str().unwrap()]);
    let repo = Repository::open(fx.path()).unwrap();
    assert!(
        repo.work_dir().is_some(),
        "a core.worktree naming itself opens"
    );
    Repository::open(fx.git_dir()).unwrap();
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
        repo.default_branch().unwrap().map(|b| b.name).as_deref(),
        Some("main"),
        "no remote HEAD, so the local mainline answers"
    );

    fx.git(&["branch", "master", "main"]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(
        repo.default_branch().unwrap().map(|b| b.name).as_deref(),
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
        repo.default_branch().unwrap().map(|b| b.name).as_deref(),
        Some("trunk"),
        "the remote's HEAD wins over the local fallback, even with no such local branch"
    );
}

#[test]
fn a_self_pointing_remote_head_falls_back_to_local_mainline() {
    let fx = Fixture::standard();
    fx.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/HEAD",
    ]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(
        repo.default_branch().unwrap().map(|b| b.name).as_deref(),
        Some("main")
    );
}

#[test]
fn a_dangling_remote_head_falls_back_to_local_mainline() {
    let fx = Fixture::standard();
    fx.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/gone",
    ]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(
        repo.default_branch().unwrap().map(|b| b.name).as_deref(),
        Some("main"),
        "a remote HEAD naming a pruned tracking branch names nothing"
    );
    let listing = repo.refs().unwrap();
    assert!(
        listing
            .broken
            .iter()
            .any(|b| b.full_name == "refs/remotes/origin/HEAD")
    );
    assert!(
        listing
            .refs
            .iter()
            .any(|r| r.full_name == "refs/heads/main")
    );
}

#[test]
fn origin_head_wins_over_other_remotes() {
    let fx = Fixture::standard();
    fx.git(&["update-ref", "refs/remotes/aaa/dev", "main"]);
    fx.git(&["update-ref", "refs/remotes/origin/trunk", "main"]);
    fx.git(&[
        "symbolic-ref",
        "refs/remotes/aaa/HEAD",
        "refs/remotes/aaa/dev",
    ]);
    fx.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(
        repo.default_branch().unwrap().map(|b| b.name).as_deref(),
        Some("trunk"),
        "origin answers even though aaa sorts first"
    );

    fx.git(&["update-ref", "refs/remotes/upstream/y", "main"]);
    fx.git(&[
        "symbolic-ref",
        "refs/remotes/upstream/HEAD",
        "refs/remotes/upstream/y",
    ]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(
        repo.default_branch().unwrap().map(|b| b.name).as_deref(),
        Some("trunk"),
        "origin answers even though upstream sorts last"
    );

    fx.git(&["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(
        repo.default_branch().unwrap().map(|b| b.name).as_deref(),
        Some("dev"),
        "without origin, the first remote in sort order answers"
    );
}

#[test]
fn a_detached_remote_head_falls_back_to_local_mainline() {
    let fx = Fixture::standard();
    fx.git(&[
        "update-ref",
        "--no-deref",
        "refs/remotes/origin/HEAD",
        "main",
    ]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(
        repo.default_branch().unwrap().map(|b| b.name).as_deref(),
        Some("main")
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
fn packed_refs_list_and_peel_like_loose_ones() {
    let fx = Fixture::standard();
    fx.git(&["pack-refs", "--all"]);
    assert!(
        !fx.git_dir().join("refs/tags/v1.0").exists(),
        "the tag lives only in packed-refs"
    );
    let repo = Repository::open(fx.path()).unwrap();
    let listing = repo.refs().unwrap();
    assert_eq!(listing.broken.len(), 0);
    assert_eq!(listing.refs.len(), 5);

    let find = |name: &str| listing.refs.iter().find(|r| r.full_name == name).unwrap();
    let expected = [
        ("refs/heads/main", RefKind::LocalBranch, "main", "main"),
        (
            "refs/heads/feature",
            RefKind::LocalBranch,
            "feature",
            "feature",
        ),
        (
            "refs/remotes/origin/main",
            RefKind::RemoteBranch,
            "origin/main",
            "origin/main",
        ),
        ("refs/tags/v0.9", RefKind::Tag, "v0.9", "v0.9"),
        ("refs/tags/v1.0", RefKind::Tag, "v1.0", "v1.0^{commit}"),
    ];
    for (full_name, kind, short_name, target) in expected {
        let r = find(full_name);
        assert_eq!(
            (r.kind, r.short_name.as_str(), r.target),
            (kind, short_name, oid(&fx.rev_parse(target))),
            "{full_name}"
        );
    }
    assert_ne!(
        find("refs/tags/v1.0").target,
        oid(&fx.rev_parse("v1.0")),
        "peeled through the packed tag, not the tag object itself"
    );
}

#[test]
fn a_packed_tag_takes_its_target_from_the_recorded_peeled_line() {
    let fx = Fixture::standard();
    fx.git(&["pack-refs", "--all"]);
    let tagged = fx.rev_parse("v1.0^{commit}");
    let elsewhere = fx.rev_parse("v0.9");
    assert_ne!(tagged, elsewhere);
    let packed = std::fs::read(fx.git_dir().join("packed-refs")).unwrap();
    let recorded = format!("^{tagged}\n");
    assert_eq!(
        packed.find_iter(recorded.as_bytes()).count(),
        1,
        "the fixture's packed-refs records v1.0's peeled id once"
    );
    let packed = packed.replace(recorded.as_bytes(), format!("^{elsewhere}\n").as_bytes());
    fx.write(".git/packed-refs", &packed);

    let repo = Repository::open(fx.path()).unwrap();
    let listing = repo.refs().unwrap();
    let tag = listing
        .refs
        .iter()
        .find(|r| r.full_name == "refs/tags/v1.0")
        .unwrap();
    assert_eq!(
        tag.target,
        oid(&elsewhere),
        "the ^ line is read instead of peeling the tag object again"
    );
}

#[test]
fn a_non_utf8_ref_name_is_listed_as_inexact() {
    let fx = Fixture::standard();
    fx.git(&["pack-refs", "--all"]);
    let mut packed = std::fs::read(fx.git_dir().join("packed-refs")).unwrap();
    packed.extend_from_slice(format!("{} refs/tags/v", fx.rev_parse("main")).as_bytes());
    packed.extend_from_slice(b"\xe9\n");
    fx.write(".git/packed-refs", &packed);

    let repo = Repository::open(fx.path()).unwrap();
    let listing = repo.refs().unwrap();
    let raw = listing
        .refs
        .iter()
        .find(|r| r.full_name == "refs/tags/v\u{fffd}")
        .unwrap();
    assert!(
        !raw.exact_name,
        "the repaired name must not go back as a ref name"
    );
    assert_eq!(raw.short_name, "v\u{fffd}");
    assert!(
        listing
            .refs
            .iter()
            .filter(|r| r.full_name != raw.full_name)
            .all(|r| r.exact_name)
    );
}

#[test]
fn a_non_utf8_remote_head_target_names_an_inexact_default_branch() {
    let fx = Fixture::standard();
    fx.git(&["pack-refs", "--all"]);
    // Appended out of order, so the header must stop promising a sorted file.
    let packed = std::fs::read(fx.git_dir().join("packed-refs")).unwrap();
    let mut packed = packed.replace(b" sorted", b"");
    packed.extend_from_slice(format!("{} refs/remotes/origin/tr", fx.rev_parse("main")).as_bytes());
    packed.extend_from_slice(b"\xe9\n");
    fx.write(".git/packed-refs", &packed);
    fx.write(
        ".git/refs/remotes/origin/HEAD",
        b"ref: refs/remotes/origin/tr\xe9\n",
    );

    let repo = Repository::open(fx.path()).unwrap();
    let branch = repo.default_branch().unwrap().unwrap();
    assert_eq!(branch.name, "tr\u{fffd}");
    assert!(
        !branch.exact_name,
        "the repaired name must not go back as a branch to check out"
    );

    fx.git(&["update-ref", "refs/remotes/origin/trunk", "main"]);
    fx.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    let repo = Repository::open(fx.path()).unwrap();
    let branch = repo.default_branch().unwrap().unwrap();
    assert_eq!(branch.name, "trunk");
    assert!(branch.exact_name);
}

#[test]
fn a_non_utf8_head_branch_is_inexact() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    assert!(repo.head().unwrap().exact_ref_name);

    fx.write(".git/HEAD", b"ref: refs/heads/ma\xe9\n");
    let repo = Repository::open(fx.path()).unwrap();
    let head = repo.head().unwrap();
    assert_eq!(head.ref_name.as_deref(), Some("refs/heads/ma\u{fffd}"));
    assert!(
        !head.exact_ref_name,
        "the repaired name must not go back as a ref name"
    );

    fx.git(&["checkout", "-q", "--detach", "main"]);
    let repo = Repository::open(fx.path()).unwrap();
    assert!(
        repo.head().unwrap().exact_ref_name,
        "a detached HEAD names no branch, so nothing is inexact"
    );
}

#[test]
fn refs_list_stash_and_notes_as_other_and_follow_remote_head() {
    let fx = Fixture::standard();
    fx.write("README.md", b"changed\n");
    fx.git(&["stash", "-q"]);
    fx.git(&["notes", "add", "-m", "n", "HEAD"]);
    fx.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    let repo = Repository::open(fx.path()).unwrap();
    let listing = repo.refs().unwrap();
    assert_eq!(listing.broken.len(), 0);

    let find = |name: &str| listing.refs.iter().find(|r| r.full_name == name).unwrap();
    let stash = find("refs/stash");
    assert_eq!(
        (stash.kind, stash.short_name.as_str(), stash.target),
        (RefKind::Other, "stash", oid(&fx.rev_parse("refs/stash")))
    );
    let notes = find("refs/notes/commits");
    assert_eq!(
        (notes.kind, notes.short_name.as_str(), notes.target),
        (
            RefKind::Other,
            "notes/commits",
            oid(&fx.rev_parse("refs/notes/commits"))
        )
    );
    let remote_head = find("refs/remotes/origin/HEAD");
    assert_eq!(
        (remote_head.kind, remote_head.short_name.as_str()),
        (RefKind::RemoteBranch, "origin/HEAD")
    );
    assert_eq!(
        remote_head.target,
        find("refs/remotes/origin/main").target,
        "followed to the branch it names"
    );
}

#[test]
fn other_ref_short_names_drop_only_the_refs_prefix() {
    let fx = Fixture::standard();
    for name in [
        "refs/stash",
        "refs/pull/1/head",
        "refs/notes/commits",
        "refs/bisect/bad",
    ] {
        fx.git(&["update-ref", name, "HEAD"]);
    }
    let repo = Repository::open(fx.path()).unwrap();
    let listing = repo.refs().unwrap();
    let mut other: Vec<(&str, &str)> = listing
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Other)
        .map(|r| (r.full_name.as_str(), r.short_name.as_str()))
        .collect();
    other.sort_unstable();
    assert_eq!(
        other,
        [
            ("refs/bisect/bad", "bisect/bad"),
            ("refs/notes/commits", "notes/commits"),
            ("refs/pull/1/head", "pull/1/head"),
            ("refs/stash", "stash"),
        ]
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
fn an_unparsable_ref_file_is_reported_without_hiding_the_rest() {
    let fx = Fixture::standard();
    fx.write(".git/refs/heads/crashed", b"");
    fx.write(".git/refs/heads/junk", b"not a ref\n");
    fx.write(".git/refs/remotes/origin/zero", b"");
    let repo = Repository::open(fx.path()).unwrap();
    let listing = repo.refs().unwrap();
    let mut broken: Vec<&str> = listing
        .broken
        .iter()
        .map(|b| b.full_name.as_str())
        .collect();
    broken.sort_unstable();
    assert_eq!(
        broken,
        [
            "refs/heads/crashed",
            "refs/heads/junk",
            "refs/remotes/origin/zero"
        ]
    );
    assert!(
        listing
            .broken
            .iter()
            .all(|b| b.error.contains(&b.full_name)),
        "{:?}",
        listing.broken
    );
    assert!(
        listing
            .refs
            .iter()
            .any(|r| r.full_name == "refs/heads/main")
    );
    assert_eq!(
        repo.default_branch().unwrap().map(|b| b.name).as_deref(),
        Some("main")
    );
}

#[test]
fn a_corrupt_object_behind_a_ref_names_the_real_cause() {
    let fx = Fixture::standard();
    let main = fx.rev_parse("main");
    let object = format!(".git/objects/{}/{}", &main[..2], &main[2..]);
    fx.set_mode(&object, 0o644);
    fx.write(&object, b"garbage\n");
    let repo = Repository::open(fx.path()).unwrap();
    let listing = repo.refs().unwrap();
    let main_ref = listing
        .broken
        .iter()
        .find(|b| b.full_name == "refs/heads/main")
        .unwrap_or_else(|| panic!("main not broken: {:?}", listing.broken));
    assert!(
        main_ref.error.contains("decompression"),
        "{}",
        main_ref.error
    );
}

/// A loose object whose zlib stream declares a stored block of `declared` bytes but ends after
/// `data`: the object header inflates, the full object does not.
fn truncated_loose_object(data: &[u8], declared: u16) -> Vec<u8> {
    let mut out = vec![0x78, 0x01, 0x01];
    out.extend_from_slice(&declared.to_le_bytes());
    out.extend_from_slice(&(!declared).to_le_bytes());
    out.extend_from_slice(data);
    out
}

#[test]
fn a_ref_whose_object_body_is_unreadable_still_lists_by_its_header() {
    let fx = Fixture::standard();
    let id = "ab".repeat(20);
    fx.write(
        &format!(".git/objects/{}/{}", &id[..2], &id[2..]),
        &truncated_loose_object(b"commit 200\0tree ", 400),
    );
    fx.write(".git/refs/heads/truncated", format!("{id}\n").as_bytes());
    let repo = Repository::open(fx.path()).unwrap();
    let listing = repo.refs().unwrap();
    assert_eq!(listing.broken, [], "only the header is read for a commit");
    let truncated = listing
        .refs
        .iter()
        .find(|r| r.full_name == "refs/heads/truncated")
        .unwrap();
    assert_eq!(truncated.target, oid(&id));
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
    assert!(!msg.contains("couldn't parse revision"), "{msg}");
    assert_no_source_location(&msg);
}

#[test]
fn resolve_errors_name_the_reason_without_parser_internals() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    for (spec, reason) in [
        ("HEAD~99999", "ancestor number 99999 is out of range"),
        ("HEAD^5", "parent number 5 is out of range"),
        ("main:nope.txt", "Could not find path \"nope.txt\""),
        ("v1.0^{blob}", "while trying to peel to blob"),
    ] {
        let err = repo.resolve(spec).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.starts_with(&format!("'{spec}' is not a revision in this repository: ")),
            "{msg}"
        );
        assert!(msg.contains(reason), "{msg}");
        for internal in ["delegate", "NthAncestor", "unknown navigation", "failed:"] {
            assert!(!msg.contains(internal), "{internal} in {msg}");
        }
        assert_no_source_location(&msg);
        assert!(
            std::error::Error::source(&err).is_some(),
            "gix cause kept for logging: {spec}"
        );
    }
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
fn resolve_tree_rejects_tags_that_end_at_a_blob() {
    let fx = Fixture::standard();
    let blob = fx.rev_parse("main:README.md");
    fx.git(&["tag", "-a", "-m", "blob", "blobtag", &blob]);
    fx.git(&["tag", "-a", "-m", "nested", "nestedtag", "blobtag"]);
    let repo = Repository::open(fx.path()).unwrap();
    for spec in ["blobtag", "nestedtag"] {
        let err = repo.resolve_tree(spec).unwrap_err();
        assert!(
            matches!(
                err,
                Error::WrongKind {
                    expected: ObjectKind::Tree,
                    actual: ObjectKind::Blob,
                    ..
                }
            ),
            "{spec}: {err}"
        );
    }
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
        Error::WrongKind {
            op: "read tree",
            expected: ObjectKind::Tree,
            actual: ObjectKind::Blob,
            ..
        }
    ));
    assert!(matches!(
        repo.entry_at_path(blob, "x").unwrap_err(),
        Error::WrongKind {
            expected: ObjectKind::Tree,
            ..
        }
    ));
}

#[test]
fn reads_of_an_absent_object_name_the_operation_and_oid() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let hex = "1".repeat(40);
    let absent = oid(&hex);
    let errors = [
        (
            "read blob header",
            repo.read_blob(absent, DEFAULT_BLOB_LIMIT).unwrap_err(),
        ),
        ("read blob header", repo.blob_size(absent).unwrap_err()),
        ("read object header", repo.commit(absent).unwrap_err()),
        (
            "read object header",
            repo.commit_details(absent).unwrap_err(),
        ),
        ("read object header", repo.log(absent, 0, 10).unwrap_err()),
        ("read object header", repo.tree_of(absent).unwrap_err()),
        ("read tree", repo.tree_entries(absent).unwrap_err()),
        (
            "read tree",
            repo.entry_at_path(absent, "README.md").unwrap_err(),
        ),
    ];
    for (op, err) in errors {
        assert!(
            matches!(err, Error::Object { oid, .. } if oid == absent),
            "{err}"
        );
        let msg = err.to_string();
        assert!(msg.starts_with(&format!("{op}: object {hex}: ")), "{msg}");
        assert!(!msg.contains("missing or unreadable"), "{msg}");
    }
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
fn an_empty_blob_is_text_under_a_zero_limit() {
    let fx = Fixture::standard();
    fx.write("empty.txt", b"");
    fx.commit("Add an empty file", "2024-01-05T00:00:00Z");
    let repo = Repository::open(fx.path()).unwrap();
    let empty = oid(&fx.rev_parse("HEAD:empty.txt"));

    assert_eq!(
        repo.read_blob(empty, 0).unwrap(),
        BlobContent::Text {
            data: vec![],
            line_starts: vec![],
            valid_utf8: true,
        }
    );
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
    let exact = README.len() as u64;
    assert!(
        matches!(
            repo.read_blob(readme, exact).unwrap(),
            BlobContent::Text { .. }
        ),
        "a blob of exactly the limit is decoded"
    );
    assert_eq!(
        repo.read_blob(readme, exact - 1).unwrap(),
        BlobContent::TooLarge {
            size: 17,
            limit: 16
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
        Error::WrongKind {
            expected: ObjectKind::Blob,
            actual: ObjectKind::Tree,
            ..
        }
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
        Error::WrongKind {
            expected: ObjectKind::Commit,
            actual: ObjectKind::Tree,
            ..
        }
    ));
}

#[test]
fn log_follows_an_annotated_tag_tip() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    let tag = repo.resolve("v1.0").unwrap();
    let commit = repo.resolve("v1.0^{commit}").unwrap();
    assert_ne!(tag, commit, "v1.0 is an annotated tag");
    let expected = repo.log(commit, 0, 10).unwrap();
    assert!(!expected.is_empty());
    assert_eq!(repo.log(tag, 0, 10).unwrap(), expected);
    assert_eq!(repo.commit(tag).unwrap(), expected[0]);
    assert_eq!(
        repo.commit_details(tag).unwrap(),
        repo.commit_details(commit).unwrap()
    );

    let blob = repo.resolve("main:README.md").unwrap();
    assert!(matches!(
        repo.log(blob, 0, 10).unwrap_err(),
        Error::WrongKind {
            expected: ObjectKind::Commit,
            actual: ObjectKind::Blob,
            ..
        }
    ));
    let tree = repo.resolve_tree("main").unwrap();
    let err = repo.log(tree, 0, 10).unwrap_err();
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
fn log_reports_a_missing_commit_as_a_walk_error() {
    let fx = Fixture::standard();
    let first = fx.rev_parse("main~1");
    let loose = format!(".git/objects/{}/{}", &first[..2], &first[2..]);
    assert!(fx.path().join(&loose).exists(), "fixture commits are loose");
    fx.remove(&loose);
    let repo = Repository::open(fx.path()).unwrap();
    let main = repo.resolve("main").unwrap();
    let cursor = repo.log_cursor(main).unwrap();
    let err = cursor.next_page(&repo, 10).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Git {
                op: "walk commits",
                ..
            }
        ),
        "{err}"
    );
    let err = cursor.next_page(&repo, 10).unwrap_err();
    assert_eq!(
        err.to_string(),
        "resume commit walk: an earlier page of this walk failed; start a new walk",
        "a cursor that lost its place refuses instead of skipping a commit"
    );
    let err = repo.log(main, 0, 10).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Git {
                op: "walk commits",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn log_cursor_pages_concatenate_to_one_walk() {
    let fx = Fixture::standard();
    fx.commit_with_body("X", "parent", "2024-03-01T00:00:00Z");
    fx.git(&["branch", "side"]);
    fx.commit_with_body("C1", "newer child", "2024-04-01T00:00:00Z");
    fx.git(&["checkout", "-q", "side"]);
    fx.commit_with_body("C2", "child older than its parent", "2024-02-01T00:00:00Z");
    fx.commit_with_body("C3", "side tip", "2024-04-15T00:00:00Z");
    fx.git(&["checkout", "-q", "main"]);
    let date = "2024-05-01T00:00:00Z";
    fx.git_with_env(
        &["merge", "-q", "--no-ff", "-m", "M", "side"],
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
    let repo = Repository::open(fx.path()).unwrap();
    let main = repo.resolve("main").unwrap();
    let whole = repo
        .log_cursor(main)
        .unwrap()
        .next_page(&repo, 1000)
        .unwrap();
    let expected = fx.git_output(&["rev-list", "--date-order", "main"]);
    assert_eq!(
        whole.len(),
        expected.lines().count(),
        "every reachable commit once"
    );

    for size in [1, 2, 3] {
        let cursor = repo.log_cursor(main).unwrap();
        let mut paged = Vec::new();
        loop {
            let page = cursor.next_page(&repo, size).unwrap();
            assert!(page.len() <= size);
            let done = page.len() < size;
            paged.extend(page);
            if done {
                break;
            }
        }
        assert_eq!(paged, whole, "pages of {size} concatenate to one walk");
    }

    let cursor = repo.log_cursor(main).unwrap();
    assert_eq!(cursor.skip(3).unwrap(), 3);
    assert_eq!(cursor.next_page(&repo, 2).unwrap(), whole[3..5]);
    assert_eq!(
        cursor.skip(1000).unwrap(),
        whole.len() - 5,
        "skip stops at the end"
    );
    assert!(cursor.next_page(&repo, 10).unwrap().is_empty());
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
fn log_orders_by_commit_time_when_a_parent_is_newer_than_its_child() {
    let fx = Fixture::standard();
    fx.commit_with_body("X", "parent", "2024-03-01T00:00:00Z");
    fx.git(&["branch", "side"]);
    fx.commit_with_body("C1", "newer child", "2024-04-01T00:00:00Z");
    fx.git(&["checkout", "-q", "side"]);
    fx.commit_with_body("C2", "child older than its parent", "2024-02-01T00:00:00Z");
    fx.git(&["checkout", "-q", "main"]);
    let date = "2024-05-01T00:00:00Z";
    fx.git_with_env(
        &["merge", "-q", "--no-ff", "-m", "M", "side"],
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
    let repo = Repository::open(fx.path()).unwrap();
    let main = repo.resolve("main").unwrap();

    let commits = repo.log(main, 0, 10).unwrap();
    let titles: Vec<_> = commits.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(
        titles,
        ["M", "C1", "X", "C2", COMMIT_2_TITLE, COMMIT_1_TITLE],
        "newest commit time first, not topological"
    );
    assert_eq!(commits[3].parents, [oid(&fx.rev_parse("main~2"))]);
    assert_eq!(
        commits[2].oid,
        oid(&fx.rev_parse("main~2")),
        "X precedes its child C2"
    );
}

#[test]
fn log_title_folds_a_wrapped_subject() {
    let fx = Fixture::standard();
    fx.write("wrapped.txt", b"x\n");
    fx.commit("first line\r\nsecond line\n\nBody", "2024-01-05T00:00:00Z");
    let repo = Repository::open(fx.path()).unwrap();
    let head = repo.resolve("HEAD").unwrap();
    let commits = repo.log(head, 0, 1).unwrap();
    assert_eq!(commits[0].title, "first line second line");
    assert_eq!(repo.commit_details(head).unwrap().body, "Body");
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
        Error::WrongKind {
            expected: ObjectKind::Commit,
            actual: ObjectKind::Tree,
            ..
        }
    ));
}

#[test]
fn log_reports_non_utc_offsets() {
    let fx = Fixture::standard();
    fx.write("east.txt", b"east\n");
    fx.commit("East", "2024-01-05T00:00:00+0530");
    fx.write("west.txt", b"west\n");
    fx.commit("West", "2024-01-06T00:00:00-0800");
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let commits = repo.log(tip, 0, 2).unwrap();
    let west = &commits[0];
    let east = &commits[1];
    assert_eq!((west.title.as_str(), west.author_offset), ("West", -28_800));
    assert_eq!((east.title.as_str(), east.author_offset), ("East", 19_800));
    assert_eq!(west.author_time, 1_704_528_000, "2024-01-06T08:00:00Z");
    assert_eq!(east.author_time, 1_704_393_000, "2024-01-04T18:30:00Z");

    let west_detail = repo.commit_details(west.oid).unwrap();
    assert_eq!(west_detail.committer_offset, -28_800);
    assert_eq!(
        repo.commit_details(east.oid).unwrap().committer_offset,
        19_800
    );
}

#[test]
fn offsets_are_seconds_east_of_utc_and_kept_apart() {
    let fx = Fixture::standard();
    fx.git_with_env(
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "tz",
            "--date",
            "2024-01-05T07:00:00+0700",
        ],
        &[("GIT_COMMITTER_DATE", "2024-01-05T00:00:00-0130")],
    );
    let repo = Repository::open(fx.path()).unwrap();
    let tip = repo.resolve("main").unwrap();

    let summary = repo.commit(tip).unwrap();
    assert_eq!(
        (summary.author_time, summary.author_offset),
        (1_704_412_800, 25_200)
    );
    let detail = repo.commit_details(tip).unwrap();
    assert_eq!(
        (detail.committer_time, detail.committer_offset),
        (1_704_418_200, -5_400)
    );
    assert_ne!(summary.author_time, detail.committer_time);

    // gix reads a malformed offset as UTC and keeps the seconds.
    let repo = raw_signature_commit(&fx, "short-offset", "1704412800 +07");
    let short = repo.resolve("short-offset").unwrap();
    let summary = repo.commit(short).unwrap();
    assert_eq!(
        (summary.author_time, summary.author_offset),
        (1_704_412_800, 0)
    );
    let detail = repo.commit_details(short).unwrap();
    assert_eq!(
        (detail.committer_time, detail.committer_offset),
        (1_704_412_800, 0)
    );

    // A time with no leading digits cannot be parsed at all: the fallback reports epoch and UTC.
    let repo = raw_signature_commit(&fx, "no-seconds", "- +0700");
    let unparsable = repo.resolve("no-seconds").unwrap();
    let summary = repo.commit(unparsable).unwrap();
    assert_eq!((summary.author_time, summary.author_offset), (0, 0));
    let detail = repo.commit_details(unparsable).unwrap();
    assert_eq!((detail.committer_time, detail.committer_offset), (0, 0));
}

/// Writes a commit on top of `main` whose author and committer lines end in `time`, bypassing
/// git's checks, points `branch` at it, and reopens the repository.
fn raw_signature_commit(fx: &Fixture, branch: &str, time: &str) -> Repository {
    let raw = format!(
        "tree {}\nparent {}\nauthor A <a@example.com> {time}\n\
         committer C <c@example.com> {time}\n\nraw signature\n",
        fx.rev_parse("main^{tree}"),
        fx.rev_parse("main"),
    );
    fx.write("raw-commit", raw.as_bytes());
    let oid = fx.git_output(&[
        "hash-object",
        "-t",
        "commit",
        "--literally",
        "-w",
        "raw-commit",
    ]);
    fx.git(&["update-ref", &format!("refs/heads/{branch}"), &oid]);
    Repository::open(fx.path()).unwrap()
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
