//! Ported from the bridge tests in `RepositoryGuardTests.swift` (`RepositoryGuardTests` and
//! `RemotePreflightTests`): `LiveActions` driven through the `RepositoryActions` seam against
//! real fixtures, plus the pure plans read through kit's one policy. The value tests of that
//! file live in `guard_bridge.rs`.
#![expect(
    clippy::unwrap_used,
    reason = "the helpers read fixtures that always exist"
)]

use rc_git_core::{BlobDiff, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT, Oid, StashEntry};
use rc_git_guard::Risk;
use rc_git_guard::actions::{
    ActionError, Author, CommitRequest, FileSelection, HunkStamp, Identity, StashRequest,
};
use rc_git_kit::guard_bridge::{CancelToken, DecisionKind, FailureKind, classify};
use rc_git_kit::repository::{LiveRepository, RepositoryReading};
use rc_git_kit::repository_actions::{
    LiveActions, RemoteRequest, RepositoryActions, plan_commit, plan_discard, plan_stash_apply,
    plan_stash_drop, plan_stash_pop, plan_stash_push, plan_tag_delete,
};
use rc_git_testkit::Fixture;

fn rev(fixture: &Fixture, spec: &str) -> Oid {
    Oid::from_hex(&fixture.rev_parse(spec)).unwrap()
}

fn actions(fixture: &Fixture) -> LiveActions {
    LiveActions::new(fixture.path())
}

fn bridge_user() -> Author {
    Author {
        explicit: None,
        fallback: Some(Identity {
            name: "Bridge User".to_owned(),
            email: "bridge@example.com".to_owned(),
        }),
    }
}

fn whole(path: &str) -> Vec<FileSelection> {
    vec![FileSelection::Whole {
        path: path.to_owned(),
    }]
}

fn status(fixture: &Fixture) -> String {
    fixture
        .git_output(&["status", "--porcelain"])
        .trim()
        .to_owned()
}

fn first_stash(fixture: &Fixture) -> StashEntry {
    let repo = LiveRepository::open(fixture.path()).unwrap();
    repo.stash_list().unwrap().into_iter().next().unwrap()
}

/// The hunks of `path` on disk against `HEAD`, as the Changes pane stamps them.
fn worktree_stamps(fixture: &Fixture, path: &str) -> Vec<HunkStamp> {
    let repo = LiveRepository::open(fixture.path()).unwrap();
    let old = repo.resolve(&format!("HEAD:{path}")).unwrap();
    let diff = repo
        .diff_worktree_file(path, Some(old), DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)
        .unwrap();
    let BlobDiff::Text { hunks } = diff else {
        return Vec::new();
    };
    hunks.iter().map(HunkStamp::from).collect()
}

#[test]
fn creates_plans_and_deletes_with_backups() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    live.create_branch("topic", "main~1").unwrap();
    assert_eq!(
        fixture.rev_parse("refs/heads/topic"),
        fixture.rev_parse("main~1")
    );
    let duplicate = live.create_branch("topic", "main").unwrap_err();
    assert!(
        duplicate.to_string().contains("refs/heads/topic"),
        "{duplicate}"
    );

    let plan = live.plan_branch_delete("feature").unwrap();
    assert_eq!(plan.kind, DecisionKind::Confirm);
    assert_eq!(plan.risk, Risk::Destructive);
    assert_eq!(plan.typed_phrase.as_deref(), Some("feature"));
    assert!(plan.backup_ref);
    assert!(!plan.merged);

    let refused = live.delete_branch("feature", Some("wrong")).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("typed confirmation did not match"),
        "{refused}"
    );
    let old_tip = fixture.rev_parse("refs/heads/feature");
    let deleted = live.delete_branch("feature", Some("feature")).unwrap();
    let backup = deleted.backup_ref.unwrap();
    assert_eq!(backup, "refs/rc-git/backup/branch/feature");
    assert_eq!(fixture.rev_parse(&backup), old_tip);

    let denied = live.plan_branch_delete("main").unwrap();
    assert_eq!(denied.kind, DecisionKind::Deny);
    assert!(denied.reason.unwrap().contains("protected"));

    assert_eq!(
        plan_tag_delete("v1.0").typed_phrase.as_deref(),
        Some("v1.0")
    );
    // The backup stores the unpeeled target: the annotated tag object, not its commit.
    let old_tag = fixture.rev_parse("refs/tags/v1.0");
    let unpeeled = live
        .delete_tag("v1.0", rev(&fixture, "refs/tags/v1.0"), Some("v1.0"))
        .unwrap_err();
    assert_eq!(classify(&unpeeled), FailureKind::Stale);
    assert_eq!(fixture.rev_parse("refs/tags/v1.0"), old_tag);
    let shown = rev(&fixture, "refs/tags/v1.0^{}");
    let tag_backup = live.delete_tag("v1.0", shown, Some("v1.0")).unwrap();
    let tag_backup = tag_backup.backup_ref.unwrap();
    assert_eq!(tag_backup, "refs/rc-git/backup/tag/v1.0");
    assert_eq!(fixture.rev_parse(&tag_backup), old_tag);
}

#[test]
fn plans_carry_the_changes_tab_policy() {
    assert_eq!(plan_commit(2).kind, DecisionKind::Allow);
    assert_eq!(plan_stash_apply().kind, DecisionKind::Allow);
    let push = plan_stash_push(3);
    assert_eq!(push.kind, DecisionKind::Confirm);
    assert_eq!(push.typed_phrase, None);
    let entry = StashEntry {
        index: 1,
        oid: Oid::from_bytes(&[0; 20]).unwrap(),
        message: "On main: wip".to_owned(),
    };
    let pop = plan_stash_pop(&entry);
    assert_eq!(pop.kind, DecisionKind::Confirm);
    assert!(pop.backup_ref);
    assert!(
        pop.summary.contains("stash@{1} \"On main: wip\""),
        "{}",
        pop.summary
    );
    let drop = plan_stash_drop(&entry);
    assert_eq!(drop.typed_phrase.as_deref(), Some("drop"));
    let discard = plan_discard(4);
    assert_eq!(discard.kind, DecisionKind::Confirm);
    assert_eq!(discard.typed_phrase, None);
    assert_eq!(discard.risk, Risk::Destructive);
}

#[test]
fn commits_stashes_and_discards_through_the_seam() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    fixture.write("alpha.txt", b"bridge edit\n");
    let head = rev(&fixture, "HEAD");
    let selections = whole("alpha.txt");
    let request = CommitRequest {
        message: "Bridge commit",
        author: bridge_user(),
        expected_head: Some(head),
        selections: &selections,
    };
    let committed = live.commit(request, None).unwrap();
    assert_eq!(committed.branch, "main");
    assert_eq!(committed.oid, rev(&fixture, "HEAD"));
    assert!(committed.index_refreshed && committed.reflog_written);
    assert_eq!(
        fixture
            .git_output(&["log", "-1", "--format=%an|%ae|%s"])
            .trim(),
        "Bridge User|bridge@example.com|Bridge commit"
    );
    assert_eq!(status(&fixture), "");

    fixture.write("alpha.txt", b"stash via bridge\n");
    let paths = ["alpha.txt".to_owned()];
    let stash = StashRequest {
        paths: &paths,
        message: Some("bridged"),
        author: bridge_user(),
    };
    live.stash_push(stash, None).unwrap();
    let shown = first_stash(&fixture);
    assert_eq!(shown.message, "On main: bridged");
    let elsewhere = StashEntry {
        oid: head,
        ..shown.clone()
    };
    let stale = live.stash_drop(&elsewhere, Some("drop")).unwrap_err();
    assert_eq!(classify(&stale), FailureKind::Stale);
    let stash_tip = fixture.rev_parse("stash@{0}");
    let popped = live.stash_pop(&shown, None).unwrap();
    let backup = popped.backup_ref.unwrap();
    assert_eq!(backup, "refs/rc-git/backup/stash/0");
    assert_eq!(fixture.rev_parse(&backup), stash_tip);
    assert_eq!(status(&fixture), "M alpha.txt");

    live.discard_files(&paths, None).unwrap();
    assert_eq!(status(&fixture), "");
}

#[test]
fn creates_a_tag_and_applies_and_drops_a_stash() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    live.create_tag("v2.0", "main~1").unwrap();
    assert_eq!(
        fixture.rev_parse("refs/tags/v2.0"),
        fixture.rev_parse("main~1")
    );
    assert!(live.create_tag("v2.0", "main").is_err());

    fixture.write("alpha.txt", b"stashed\n");
    fixture.git(&["stash", "push", "-q", "-m", "kept"]);
    let entry = first_stash(&fixture);
    live.stash_apply(&entry, None).unwrap();
    assert_eq!(
        std::fs::read(fixture.path().join("alpha.txt")).unwrap(),
        b"stashed\n"
    );
    let dropped = live.stash_drop(&entry, Some("drop")).unwrap();
    assert_eq!(
        dropped.backup_ref.as_deref(),
        Some("refs/rc-git/backup/stash/0")
    );
    assert_eq!(fixture.git_output(&["stash", "list"]), "");
}

#[test]
fn hunk_selections_commit_and_discard() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    let lines: Vec<String> = (1..=20).map(|n| format!("l{n}\n")).collect();
    fixture.write("hunky.txt", lines.concat().as_bytes());
    fixture.commit("Add hunky", "2024-01-05T00:00:00+0000");
    let mut edited = lines;
    edited[17] = "L18a\nL18b\n".to_owned();
    edited[0] = "l1\nTOP1\nTOP2\n".to_owned();
    fixture.write("hunky.txt", edited.concat().as_bytes());

    let stamps = worktree_stamps(&fixture, "hunky.txt");
    assert_eq!(stamps.len(), 2);
    let selections = vec![FileSelection::Hunks {
        path: "hunky.txt".to_owned(),
        hunks: vec![stamps[1]],
    }];
    let request = CommitRequest {
        message: "Second hunk",
        author: bridge_user(),
        expected_head: Some(rev(&fixture, "HEAD")),
        selections: &selections,
    };
    live.commit(request, None).unwrap();
    let committed = fixture.git_output(&["show", "HEAD:hunky.txt"]);
    assert!(committed.contains("L18a\nL18b"));
    assert!(!committed.contains("TOP1"));

    let remaining = worktree_stamps(&fixture, "hunky.txt");
    assert_eq!(remaining.len(), 1);
    live.discard_hunks("hunky.txt", &remaining, None).unwrap();
    assert_eq!(status(&fixture), "");
}

#[test]
fn a_commit_against_a_moved_head_or_with_no_hunks_commits_nothing() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    fixture.write("alpha.txt", b"judged against an old head\n");
    let head = fixture.rev_parse("HEAD");
    let selections = whole("alpha.txt");
    let stale = CommitRequest {
        message: "Stale",
        author: bridge_user(),
        expected_head: Some(rev(&fixture, "HEAD~1")),
        selections: &selections,
    };
    assert_eq!(
        classify(&live.commit(stale, None).unwrap_err()),
        FailureKind::Stale
    );

    let empty = vec![FileSelection::Hunks {
        path: "alpha.txt".to_owned(),
        hunks: Vec::new(),
    }];
    let nothing = CommitRequest {
        message: "Nothing",
        author: bridge_user(),
        expected_head: Some(rev(&fixture, "HEAD")),
        selections: &empty,
    };
    let error = live.commit(nothing, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no hunks selected for 'alpha.txt'"),
        "{error}"
    );
    assert_eq!(fixture.rev_parse("HEAD"), head);
    assert_eq!(status(&fixture), "M alpha.txt");
}

#[test]
fn a_saved_fallback_never_overrides_the_repository_identity() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    assert_eq!(live.configured_identity().unwrap(), None);
    fixture.git(&["config", "user.name", "Work Me"]);
    fixture.git(&["config", "user.email", "work@example.com"]);
    let work = Identity {
        name: "Work Me".to_owned(),
        email: "work@example.com".to_owned(),
    };
    assert_eq!(live.configured_identity().unwrap(), Some(work));

    fixture.write("alpha.txt", b"work edit\n");
    let selections = whole("alpha.txt");
    let request = CommitRequest {
        message: "Work commit",
        author: bridge_user(),
        expected_head: Some(rev(&fixture, "HEAD")),
        selections: &selections,
    };
    live.commit(request, None).unwrap();
    assert_eq!(
        fixture
            .git_output(&["log", "-1", "--format=%ae|%ce"])
            .trim(),
        "work@example.com|work@example.com"
    );
}

#[test]
fn a_missing_identity_is_its_own_failure_kind() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    fixture.write("alpha.txt", b"nobody's edit\n");
    let head = fixture.rev_parse("HEAD");
    let selections = whole("alpha.txt");
    let request = CommitRequest {
        message: "Nobody",
        author: Author::default(),
        expected_head: Some(rev(&fixture, "HEAD")),
        selections: &selections,
    };
    let error = live.commit(request, None).unwrap_err();
    assert_eq!(classify(&error), FailureKind::IdentityRequired);
    assert_eq!(fixture.rev_parse("HEAD"), head);

    let paths = ["alpha.txt".to_owned()];
    let stash = StashRequest {
        paths: &paths,
        message: None,
        author: Author::default(),
    };
    let error = live.stash_push(stash, None).unwrap_err();
    assert_eq!(classify(&error), FailureKind::IdentityRequired);
    assert_eq!(fixture.git_output(&["stash", "list"]), "");
}

#[test]
fn reading_a_missing_repository_names_it() {
    let live = LiveActions::new("/nonexistent/rc-git-repo");
    let error = live.configured_identity().unwrap_err();
    assert!(
        error.to_string().contains("/nonexistent/rc-git-repo"),
        "{error}"
    );
    let error = live.default_branch().unwrap_err();
    assert!(error.to_string().contains("read default branch"), "{error}");
}

#[test]
fn checkout_switches_branches_and_refuses_a_dirty_path() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    let expected = fixture
        .git_output(&["diff", "--name-only", "main", "feature"])
        .lines()
        .count();
    let outcome = live.checkout("feature").unwrap();
    assert_eq!(outcome.branch, "feature");
    assert!(!outcome.already_on_branch);
    assert!(outcome.reflog_written);
    assert_eq!(outcome.files_changed, expected);
    assert_eq!(outcome.new_head, rev(&fixture, "feature"));
    assert!(live.checkout("feature").unwrap().already_on_branch);

    // feature.txt exists only on `feature`, so switching away must delete it.
    fixture.write("feature.txt", b"work in progress\n");
    let dirty = live.checkout("main").unwrap_err();
    assert_eq!(classify(&dirty), FailureKind::Failed);
    assert!(dirty.to_string().contains("feature.txt"), "{dirty}");
    assert_eq!(
        fixture
            .git_output(&["rev-parse", "--abbrev-ref", "HEAD"])
            .trim(),
        "feature"
    );
    assert_eq!(
        live.default_branch()
            .unwrap()
            .map(|branch| branch.name)
            .as_deref(),
        Some("main"),
        "Back to default reads the mainline through the same seam"
    );
}

/// Points `main` at an unreachable origin; nothing below touches the network.
fn configure_upstream(fixture: &Fixture) {
    fixture.git(&["remote", "add", "origin", "https://127.0.0.1:9/repo.git"]);
    fixture.git(&["config", "branch.main.remote", "origin"]);
    fixture.git(&["config", "branch.main.merge", "refs/heads/main"]);
}

/// Moves `refs/remotes/origin/main` one commit (adding `news.txt`) past `main`.
fn advance_tracking_ref(fixture: &Fixture) {
    fixture.write("news.txt", b"news\n");
    fixture.commit("Upstream", "2024-03-01T00:00:00+0000");
    fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    fixture.git(&["reset", "-q", "--hard", "HEAD~1"]);
}

#[test]
fn the_upstream_status_reads_configuration_and_counts() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    let status = live.upstream_status().unwrap();
    assert!(!status.has_upstream);
    assert_eq!(status.branch, "main");

    fixture.git(&[
        "remote",
        "add",
        "origin",
        "https://example.com:8443/repo.git",
    ]);
    fixture.git(&["config", "branch.main.remote", "origin"]);
    fixture.git(&["config", "branch.main.merge", "refs/heads/main"]);
    let status = live.upstream_status().unwrap();
    assert!(status.has_upstream && status.published && !status.uses_ssh);
    assert_eq!(status.host, "example.com:8443");
    assert_eq!((status.ahead, status.behind), (0, 0));

    fixture.git(&["commit", "--allow-empty", "-q", "-m", "local"]);
    let status = live.upstream_status().unwrap();
    assert_eq!(
        (status.ahead, status.behind),
        (1, 0),
        "the counts are not swapped"
    );

    fixture.git(&["remote", "set-url", "origin", "git@example.com:x/y.git"]);
    let status = live.upstream_status().unwrap();
    assert!(status.uses_ssh);
    assert_eq!(status.host, "example.com");
}

#[test]
fn a_fast_forward_pull_advances_and_a_diverged_one_refuses() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    configure_upstream(&fixture);
    advance_tracking_ref(&fixture);
    assert_eq!(live.upstream_status().unwrap().behind, 1);
    let outcome = live.pull_fast_forward().unwrap();
    assert_eq!(
        fixture.rev_parse("HEAD"),
        fixture.rev_parse("refs/remotes/origin/main")
    );
    assert_eq!(outcome.files_changed, 1);
    assert_eq!(live.pull_fast_forward().unwrap().files_changed, 0);

    let diverged_fixture = Fixture::standard();
    let diverged = actions(&diverged_fixture);
    configure_upstream(&diverged_fixture);
    advance_tracking_ref(&diverged_fixture);
    diverged_fixture.write("local.txt", b"local\n");
    diverged_fixture.commit("Local", "2024-03-02T00:00:00+0000");
    let head = diverged_fixture.rev_parse("HEAD");
    let error = diverged.pull_fast_forward().unwrap_err();
    assert_eq!(classify(&error), FailureKind::Diverged);
    assert!(
        error.to_string().contains("1 ahead and 1 behind"),
        "{error}"
    );
    assert_eq!(diverged_fixture.rev_parse("HEAD"), head);
}

#[test]
fn a_push_maps_a_cancel_a_refused_remote_and_a_missing_one() {
    let fixture = Fixture::standard();
    let live = actions(&fixture);
    let missing = live
        .push(&RemoteRequest::default(), &CancelToken::new())
        .unwrap_err();
    assert!(
        missing.to_string().contains("has no remote to use"),
        "{missing}"
    );

    configure_upstream(&fixture);
    let cancel = CancelToken::new();
    cancel.signal();
    let cancelled = live.push(&RemoteRequest::default(), &cancel).unwrap_err();
    assert!(
        matches!(cancelled, ActionError::Cancelled { .. }),
        "{cancelled}"
    );
    let fetch = live.fetch(&RemoteRequest::default(), &cancel).unwrap_err();
    assert_eq!(classify(&fetch), FailureKind::Cancelled);

    fixture.git(&["remote", "set-url", "origin", "git://example.com/x.git"]);
    let refused = live
        .push(&RemoteRequest::default(), &CancelToken::new())
        .unwrap_err();
    assert_eq!(classify(&refused), FailureKind::Failed);
    assert!(refused.to_string().contains("git"), "{refused}");
}
