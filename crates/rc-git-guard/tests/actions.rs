//! The write path: branch and tag creation and deletion, commit and discard, stash, and checkout,
//! each gated by the policy, with backup refs proving the safety net works.

use rc_git_guard::actions::{
    ActionError, Author, Identity, create_branch, create_tag, delete_branch, delete_tag,
    plan_branch_delete,
};
use rc_git_guard::{Decision, Policy, Risk};
use rc_git_testkit::Fixture;

fn refs(fx: &Fixture) -> String {
    fx.git_output(&["for-each-ref", "--format=%(refname)"])
}

/// What the ref list shows `tag` pointing at: its target after peeling.
#[expect(
    clippy::unwrap_used,
    reason = "test helper panics on a malformed tag target"
)]
fn shown(fx: &Fixture, tag: &str) -> rc_git_core::Oid {
    rc_git_core::Oid::from_hex(&fx.rev_parse(&format!("refs/tags/{tag}^{{}}"))).unwrap()
}

fn porcelain(fx: &Fixture) -> String {
    fx.git_output(&["status", "--porcelain"])
}

fn identity() -> Identity {
    Identity {
        name: "Test User".to_owned(),
        email: "test@example.com".to_owned(),
    }
}

/// The test identity, recorded ahead of whatever the repository's config sets.
fn explicit() -> Author {
    Author {
        explicit: Some(identity()),
        fallback: None,
    }
}

#[test]
fn a_reftable_repository_is_refused_before_any_write() {
    let Some(fx) = Fixture::reftable() else {
        return;
    };
    let before = refs(&fx);
    let err = create_branch(fx.path(), &Policy::default(), "topic", "main").unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    assert!(
        err.to_string()
            .contains("reftable repositories are not supported"),
        "{err}"
    );
    assert_eq!(refs(&fx), before, "no ref was written");
}

#[test]
fn discard_and_checkout_refuse_a_core_worktree_outside_the_repository() {
    let fx = Fixture::standard();
    let outside = Fixture::empty_dir();
    let victim = outside.path().join("victim.txt");
    outside.write("victim.txt", b"keep\n");
    let outside_path = outside.path().to_str().unwrap();
    fx.git(&["config", "core.worktree", outside_path]);
    let head = fx.rev_parse("HEAD");
    let policy = Policy::default();

    let err =
        rc_git_guard::actions::discard_files(fx.path(), &policy, &["victim.txt".to_owned()], None)
            .unwrap_err();
    assert!(
        matches!(err, ActionError::Git { op: "discard", .. }),
        "{err}"
    );
    assert!(
        err.to_string()
            .contains(&format!("core.worktree = {outside_path}")),
        "{err}"
    );

    let err = rc_git_guard::actions::checkout_branch(fx.path(), &policy, "feature").unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    assert!(err.to_string().contains("core.worktree"), "{err}");

    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"keep\n",
        "the outside file is untouched"
    );
    let names: Vec<_> = std::fs::read_dir(outside.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["victim.txt"], "nothing was checked out outside");
    assert_eq!(fx.rev_parse("HEAD"), head, "HEAD did not move");
}

#[test]
fn branches_and_tags_are_created_at_the_named_revision_and_never_overwritten() {
    let fx = Fixture::standard();
    let policy = Policy::default();

    create_branch(fx.path(), &policy, "topic", "main~1").unwrap();
    assert_eq!(fx.rev_parse("refs/heads/topic"), fx.rev_parse("main~1"));

    let err = create_branch(fx.path(), &policy, "topic", "main").unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    assert_eq!(
        fx.rev_parse("refs/heads/topic"),
        fx.rev_parse("main~1"),
        "the existing branch is untouched"
    );

    create_tag(fx.path(), &policy, "v2.0", "feature").unwrap();
    assert_eq!(fx.rev_parse("refs/tags/v2.0"), fx.rev_parse("feature"));
    assert!(create_tag(fx.path(), &policy, "v2.0", "main").is_err());

    let err = create_branch(fx.path(), &policy, "ghost", "no-such-rev").unwrap_err();
    assert!(err.to_string().contains("no-such-rev"), "{err}");
}

#[test]
fn creating_a_ref_again_at_its_own_target_is_refused_without_a_reflog_line() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    create_branch(fx.path(), &policy, "topic", "HEAD").unwrap();
    create_tag(fx.path(), &policy, "v2.0", "HEAD").unwrap();
    let before = refs(&fx);
    let branch_log = fx.git_output(&["reflog", "show", "refs/heads/topic"]);

    let err = create_branch(fx.path(), &policy, "topic", "HEAD").unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    assert!(
        err.to_string()
            .contains("create branch 'refs/heads/topic': it already exists"),
        "{err}"
    );
    let err = create_tag(fx.path(), &policy, "v2.0", "HEAD").unwrap_err();
    assert!(
        err.to_string()
            .contains("create tag 'refs/tags/v2.0': it already exists"),
        "{err}"
    );

    assert_eq!(refs(&fx), before, "no ref was written");
    assert_eq!(
        fx.git_output(&["reflog", "show", "refs/heads/topic"]),
        branch_log,
        "the branch reflog gained no line"
    );
}

#[test]
fn a_branch_is_created_at_the_commit_a_tag_names_and_never_on_a_tree() {
    let fx = Fixture::standard();
    let policy = Policy::default();

    create_branch(fx.path(), &policy, "fromtag", "v1.0").unwrap();
    assert_eq!(
        fx.git_output(&["cat-file", "-t", "refs/heads/fromtag"])
            .trim(),
        "commit"
    );
    assert_eq!(
        fx.rev_parse("refs/heads/fromtag"),
        fx.rev_parse("v1.0^{commit}")
    );

    let before = refs(&fx);
    let err = create_branch(fx.path(), &policy, "fromtree", "HEAD^{tree}").unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    let message = err.to_string();
    assert!(message.contains("create branch"), "names the op: {message}");
    assert!(
        message.contains("HEAD^{tree}"),
        "names the revision: {message}"
    );
    assert_eq!(refs(&fx), before, "no ref was written");
}

#[test]
fn an_invalid_branch_or_tag_name_is_refused_before_any_write() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let before = refs(&fx);

    for name in ["HEAD", "-dash", "a..b"] {
        let err = create_branch(fx.path(), &policy, name, "HEAD").unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{name}: {err}");
        let message = err.to_string();
        assert!(message.contains("create branch"), "names the op: {message}");
        assert!(message.contains(name), "names the branch: {message}");
    }
    for name in ["HEAD", "-dash"] {
        let err = create_tag(fx.path(), &policy, name, "HEAD").unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{name}: {err}");
        assert!(
            err.to_string().contains(&format!("create tag '{name}'")),
            "{err}"
        );
    }
    assert!(
        !fx.path().join(".git/refs/tags/HEAD").exists(),
        "no refs/tags/HEAD"
    );

    assert_eq!(refs(&fx), before, "no ref was written");
    fx.git(&["checkout", "-q", "-b", "x"]);
}

#[test]
fn deleting_a_merged_branch_needs_only_a_plain_confirm_and_no_backup() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    create_branch(fx.path(), &policy, "old-work", "main~1").unwrap();

    let plan = plan_branch_delete(fx.path(), &policy, "old-work").unwrap();
    assert!(plan.merged, "an ancestor of main is merged");
    let Decision::Confirm(challenge) = &plan.decision else {
        panic!("expected Confirm, got {:?}", plan.decision);
    };
    assert_eq!(challenge.risk(), Risk::Recoverable);
    assert!(challenge.typed_phrase().is_none());
    assert!(!challenge.backup_ref());

    let deleted = delete_branch(fx.path(), &policy, "old-work", None).unwrap();
    assert_eq!(deleted.full_name, "refs/heads/old-work");
    assert!(deleted.backup_ref.is_none());
    assert!(!refs(&fx).contains("refs/heads/old-work"));
}

#[test]
fn an_unreadable_head_fails_the_merge_check_instead_of_dropping_its_tip() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    create_branch(fx.path(), &policy, "old-work", "main~1").unwrap();
    fx.write(".git/HEAD", b"1111111111111111111111111111111111111111\n");

    let err = plan_branch_delete(fx.path(), &policy, "old-work").unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    assert!(err.to_string().contains("HEAD"), "names HEAD: {err}");
    let err = delete_branch(fx.path(), &policy, "old-work", None).unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    assert!(
        refs(&fx).contains("refs/heads/old-work"),
        "nothing was deleted"
    );
}

#[test]
fn an_unreadable_branch_tip_fails_the_merge_check_instead_of_being_skipped() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    create_branch(fx.path(), &policy, "old-work", "main~1").unwrap();
    fx.write(
        ".git/refs/heads/ghost",
        b"1111111111111111111111111111111111111111\n",
    );

    let err = plan_branch_delete(fx.path(), &policy, "old-work").unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    assert!(
        err.to_string().contains("refs/heads/ghost"),
        "names the ref: {err}"
    );
}

#[test]
fn deleting_an_unmerged_branch_demands_the_name_and_writes_a_backup() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let feature_tip = fx.rev_parse("feature");

    let plan = plan_branch_delete(fx.path(), &policy, "feature").unwrap();
    assert!(!plan.merged);
    let Decision::Confirm(challenge) = &plan.decision else {
        panic!("expected Confirm");
    };
    assert_eq!(challenge.typed_phrase(), Some("feature"));
    assert!(challenge.backup_ref());

    let err = delete_branch(fx.path(), &policy, "feature", Some("wrong")).unwrap_err();
    assert!(matches!(err, ActionError::NotConfirmed { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "delete branch 'feature': the typed confirmation did not match; \
         type 'feature' to confirm"
    );
    let listing = refs(&fx);
    assert!(listing.contains("refs/heads/feature"), "nothing happened");
    assert!(!listing.contains("refs/rc-git/backup"), "no backup either");

    let deleted = delete_branch(fx.path(), &policy, "feature", Some("feature")).unwrap();
    assert_eq!(
        deleted.backup_ref.as_deref(),
        Some("refs/rc-git/backup/branch/feature")
    );
    assert!(!refs(&fx).contains("refs/heads/feature"));
    assert_eq!(
        fx.rev_parse("refs/rc-git/backup/branch/feature"),
        feature_tip,
        "the backup points at the lost tip"
    );

    // The same name again: the next backup gets a numbered slot.
    create_branch(fx.path(), &policy, "feature", &feature_tip).unwrap();
    fx.git(&["checkout", "-q", "feature"]);
    fx.write("more.txt", b"more\n");
    fx.commit("More feature work", "2024-01-04T00:00:00+0000");
    fx.git(&["checkout", "-q", "main"]);
    let second_tip = fx.rev_parse("feature");
    let deleted = delete_branch(fx.path(), &policy, "feature", Some("feature")).unwrap();
    assert_eq!(
        deleted.backup_ref.as_deref(),
        Some("refs/rc-git/backup/branch/feature-2")
    );
    assert_eq!(
        fx.rev_parse("refs/rc-git/backup/branch/feature-2"),
        second_tip
    );
    assert_eq!(
        fx.rev_parse("refs/rc-git/backup/branch/feature"),
        feature_tip
    );
}

#[test]
fn a_branch_nested_under_a_backed_up_name_still_backs_up() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let tip = fx.rev_parse("feature");
    delete_branch(fx.path(), &policy, "feature", Some("feature")).unwrap();

    create_branch(fx.path(), &policy, "feature/x", &tip).unwrap();
    let deleted = delete_branch(fx.path(), &policy, "feature/x", Some("feature/x")).unwrap();
    assert_eq!(
        deleted.backup_ref.as_deref(),
        Some("refs/rc-git/backup/branch/feature%2Fx"),
        "the slash is escaped, so the backup cannot nest under `feature`"
    );
    assert!(!refs(&fx).contains("refs/heads/feature/x"));
    assert_eq!(fx.rev_parse("refs/rc-git/backup/branch/feature%2Fx"), tip);
    assert_eq!(fx.rev_parse("refs/rc-git/backup/branch/feature"), tip);
}

#[test]
fn a_backup_takes_the_slot_after_the_highest_existing_one() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let tip = fx.rev_parse("feature");
    let main = fx.rev_parse("main");
    fx.git(&["update-ref", "refs/rc-git/backup/branch/feature-7", &main]);
    fx.git(&["update-ref", "refs/rc-git/backup/branch/feature-x", &main]);
    fx.git(&["update-ref", "refs/rc-git/backup/branch/feature/old", &main]);

    let deleted = delete_branch(fx.path(), &policy, "feature", Some("feature")).unwrap();
    assert_eq!(
        deleted.backup_ref.as_deref(),
        Some("refs/rc-git/backup/branch/feature-8"),
        "one past the highest slot: `feature-x` is another name, and the older nested \
         `feature/old` occupies the unnumbered slot"
    );
    assert_eq!(fx.rev_parse("refs/rc-git/backup/branch/feature-8"), tip);
    assert_eq!(fx.rev_parse("refs/rc-git/backup/branch/feature-7"), main);
}

/// Creates `branch` from `main` with one commit of its own, so deleting it needs a backup, and
/// returns its tip.
fn unmerged_branch(fx: &Fixture, branch: &str, date: &str) -> String {
    fx.git(&["checkout", "-q", "-b", branch, "main"]);
    fx.write(&format!("{branch}.txt"), format!("{date}\n").as_bytes());
    fx.commit(&format!("Work on {branch}"), date);
    fx.git(&["checkout", "-q", "main"]);
    fx.rev_parse(branch)
}

#[test]
fn a_dashed_name_never_shares_a_slot_with_another_names_backup() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let first_tip = fx.rev_parse("feature");
    delete_branch(fx.path(), &policy, "feature", Some("feature")).unwrap();
    let second_tip = unmerged_branch(&fx, "feature", "2024-01-05T00:00:00+0000");
    let second = delete_branch(fx.path(), &policy, "feature", Some("feature")).unwrap();
    let dashed_tip = unmerged_branch(&fx, "feature-2", "2024-01-06T00:00:00+0000");

    let dashed = delete_branch(fx.path(), &policy, "feature-2", Some("feature-2")).unwrap();
    assert_eq!(
        second.backup_ref.as_deref(),
        Some("refs/rc-git/backup/branch/feature-2")
    );
    assert_eq!(
        dashed.backup_ref.as_deref(),
        Some("refs/rc-git/backup/branch/feature%2D2"),
        "the dash is escaped, so `feature-2` cannot land in `feature`'s second slot"
    );
    assert_eq!(fx.rev_parse("refs/rc-git/backup/branch/feature"), first_tip);
    assert_eq!(
        fx.rev_parse("refs/rc-git/backup/branch/feature-2"),
        second_tip
    );
    assert_eq!(
        fx.rev_parse("refs/rc-git/backup/branch/feature%2D2"),
        dashed_tip
    );
}

#[test]
fn a_backup_that_already_holds_the_tip_is_reused_under_either_name() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let legacy_tip = unmerged_branch(&fx, "my-work", "2024-01-05T00:00:00+0000");
    let other_tip = unmerged_branch(&fx, "other", "2024-01-06T00:00:00+0000");
    fx.git(&[
        "update-ref",
        "refs/rc-git/backup/branch/my-work",
        &legacy_tip,
    ]);
    fx.git(&["update-ref", "refs/rc-git/backup/branch/other", &other_tip]);
    let before = refs(&fx);

    let legacy = delete_branch(fx.path(), &policy, "my-work", Some("my-work")).unwrap();
    let current = delete_branch(fx.path(), &policy, "other", Some("other")).unwrap();
    assert_eq!(
        legacy.backup_ref.as_deref(),
        Some("refs/rc-git/backup/branch/my-work"),
        "the unescaped name an older version wrote is still found"
    );
    assert_eq!(
        current.backup_ref.as_deref(),
        Some("refs/rc-git/backup/branch/other")
    );
    let after = refs(&fx);
    assert_eq!(
        after.matches("refs/rc-git/backup").count(),
        before.matches("refs/rc-git/backup").count(),
        "no second slot for the same object: {after}"
    );
}

#[test]
fn an_unreadable_head_refuses_a_branch_delete() {
    let fx = Fixture::standard();
    fx.write(".git/HEAD", b"garbage\n");

    let err = delete_branch(fx.path(), &Policy::default(), "feature", Some("feature")).unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    assert!(err.to_string().contains("HEAD"), "{err}");
    fx.write(".git/HEAD", b"ref: refs/heads/main\n");
    assert!(
        refs(&fx).contains("refs/heads/feature"),
        "nothing was deleted"
    );
}

#[test]
fn protected_and_checked_out_branches_cannot_be_deleted() {
    let fx = Fixture::standard();
    let policy = Policy::default();

    let err = delete_branch(fx.path(), &policy, "main", None).unwrap_err();
    assert!(matches!(err, ActionError::CheckedOut { .. }), "{err}");

    fx.git(&["checkout", "-q", "feature"]);
    let err = delete_branch(fx.path(), &policy, "main", Some("main")).unwrap_err();
    assert!(matches!(err, ActionError::Denied { .. }), "{err}");
    assert!(err.to_string().contains("protected"));
    assert!(refs(&fx).contains("refs/heads/main"));

    let plan = plan_branch_delete(fx.path(), &policy, "main").unwrap();
    assert!(matches!(plan.decision, Decision::Deny { .. }));
}

#[test]
fn a_symbolic_alias_is_not_evidence_that_an_unmerged_branch_survives() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let feature_tip = fx.rev_parse("feature");
    fx.git(&["symbolic-ref", "refs/heads/alias", "refs/heads/feature"]);

    let plan = plan_branch_delete(fx.path(), &policy, "feature").unwrap();
    assert!(!plan.merged, "an alias of feature peels to its own tip");
    let Decision::Confirm(challenge) = &plan.decision else {
        panic!("expected Confirm, got {:?}", plan.decision);
    };
    assert_eq!(challenge.typed_phrase(), Some("feature"));
    assert!(challenge.backup_ref());

    let err = delete_branch(fx.path(), &policy, "feature", None).unwrap_err();
    assert!(matches!(err, ActionError::NotConfirmed { .. }), "{err}");
    let deleted = delete_branch(fx.path(), &policy, "feature", Some("feature")).unwrap();
    assert_eq!(
        fx.rev_parse(deleted.backup_ref.as_deref().unwrap()),
        feature_tip,
        "the backup keeps the unmerged tip"
    );
}

#[test]
fn head_through_an_alias_of_the_branch_is_not_evidence_either() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    fx.git(&["symbolic-ref", "refs/heads/alias", "refs/heads/feature"]);
    fx.git(&["symbolic-ref", "HEAD", "refs/heads/alias"]);

    let plan = plan_branch_delete(fx.path(), &policy, "feature").unwrap();
    assert!(!plan.merged, "HEAD -> alias -> feature is feature itself");
}

#[test]
fn a_branch_checked_out_through_an_alias_cannot_be_deleted() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    fx.git(&["symbolic-ref", "refs/heads/alias", "refs/heads/feature"]);
    fx.git(&["symbolic-ref", "HEAD", "refs/heads/alias"]);

    let err = delete_branch(fx.path(), &policy, "feature", Some("feature")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "cannot delete 'feature': it is the checked-out branch"
    );
    assert!(
        refs(&fx).contains("refs/heads/feature"),
        "nothing was deleted"
    );
}

#[test]
fn a_branch_checked_out_through_an_alias_in_a_linked_worktree_cannot_be_deleted() {
    let fx = Fixture::standard();
    let holder = Fixture::empty_dir();
    let wt = holder.path().join("wt");
    fx.git(&["branch", "topic", "main"]);
    fx.git(&["worktree", "add", "-q", wt.to_str().unwrap(), "topic"]);
    let wt = wt.canonicalize().unwrap();
    fx.git(&["symbolic-ref", "refs/heads/alias", "refs/heads/feature"]);
    fx.git(&[
        "-C",
        wt.to_str().unwrap(),
        "symbolic-ref",
        "HEAD",
        "refs/heads/alias",
    ]);

    let err = delete_branch(fx.path(), &Policy::default(), "feature", Some("feature")).unwrap_err();
    let ActionError::CheckedOut { worktree, .. } = &err else {
        panic!("expected CheckedOut, got {err}");
    };
    assert_eq!(worktree.as_deref(), Some(wt.to_str().unwrap()));
    assert!(
        refs(&fx).contains("refs/heads/feature"),
        "nothing was deleted"
    );
}

#[test]
fn the_only_branch_has_nowhere_else_its_commits_live() {
    let fx = Fixture::standard();
    fx.git(&["checkout", "-q", "feature"]);
    fx.git(&["branch", "-q", "-D", "main"]);

    let plan = plan_branch_delete(fx.path(), &Policy::default(), "feature").unwrap();
    assert!(
        !plan.merged,
        "with no other branch and HEAD on this one, nothing else holds its commits"
    );
}

#[test]
fn the_checked_out_branch_plans_a_deny_and_counts_head_as_itself() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    fx.git(&["checkout", "-q", "feature"]);

    let plan = plan_branch_delete(fx.path(), &policy, "feature").unwrap();
    assert!(
        !plan.merged,
        "HEAD on the branch is not another place its commits live"
    );
    let Decision::Deny { reason } = &plan.decision else {
        panic!("expected a deny, got {:?}", plan.decision);
    };
    assert_eq!(
        reason,
        "cannot delete 'feature': it is the checked-out branch"
    );
    let err = delete_branch(fx.path(), &policy, "feature", Some("feature")).unwrap_err();
    assert_eq!(err.to_string(), *reason, "plan and delete refuse alike");

    fx.git(&["checkout", "-q", "--detach", "feature"]);
    let plan = plan_branch_delete(fx.path(), &policy, "feature").unwrap();
    assert!(
        plan.merged,
        "a detached HEAD at the tip still keeps its commits"
    );
    assert!(
        matches!(plan.decision, Decision::Confirm(_)),
        "{:?}",
        plan.decision
    );
}

/// `fx` with `feature` checked out in a linked worktree under `holder`; returns its path as
/// git recorded it.
#[expect(clippy::unwrap_used, reason = "test helper panics on fixture failure")]
fn feature_in_linked_worktree(fx: &Fixture, holder: &Fixture) -> std::path::PathBuf {
    let wt = holder.path().join("wt");
    fx.git(&["worktree", "add", "-q", wt.to_str().unwrap(), "feature"]);
    wt.canonicalize().unwrap()
}

#[test]
fn a_branch_checked_out_in_a_linked_worktree_cannot_be_deleted() {
    let fx = Fixture::standard();
    let holder = Fixture::empty_dir();
    let wt = feature_in_linked_worktree(&fx, &holder);
    let policy = Policy::default();
    let expected = format!(
        "cannot delete 'feature': it is checked out in the worktree at '{}'",
        wt.display()
    );

    let plan = plan_branch_delete(fx.path(), &policy, "feature").unwrap();
    let Decision::Deny { reason } = &plan.decision else {
        panic!("expected a deny, got {:?}", plan.decision);
    };
    assert_eq!(*reason, expected);

    let err = delete_branch(fx.path(), &policy, "feature", Some("feature")).unwrap_err();
    let ActionError::CheckedOut { worktree, .. } = &err else {
        panic!("expected CheckedOut, got {err}");
    };
    assert_eq!(worktree.as_deref(), Some(wt.to_str().unwrap()));
    assert_eq!(err.to_string(), expected, "plan and delete refuse alike");
    let after = refs(&fx);
    assert!(after.contains("refs/heads/feature"), "{after}");
    assert!(!after.contains("refs/rc-git/backup"), "{after}");
}

#[test]
fn a_linked_worktree_sees_the_main_worktree_branch_and_its_own() {
    let fx = Fixture::standard();
    let holder = Fixture::empty_dir();
    let wt = feature_in_linked_worktree(&fx, &holder);
    let policy = Policy::default();
    fx.git(&["branch", "topic", "main"]);

    let err = delete_branch(&wt, &policy, "main", Some("main")).unwrap_err();
    let ActionError::CheckedOut { worktree, .. } = &err else {
        panic!("expected CheckedOut, got {err}");
    };
    let main_wt = fx.path().canonicalize().unwrap();
    assert_eq!(worktree.as_deref(), Some(main_wt.to_str().unwrap()));

    let err = delete_branch(&wt, &policy, "feature", Some("feature")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "cannot delete 'feature': it is the checked-out branch"
    );

    delete_branch(&wt, &policy, "topic", None).unwrap();
    assert!(!refs(&fx).contains("refs/heads/topic"));
}

#[test]
fn checkout_refuses_a_branch_another_worktree_has_checked_out() {
    let fx = Fixture::standard();
    let holder = Fixture::empty_dir();
    let wt = feature_in_linked_worktree(&fx, &holder);
    let policy = Policy::default();

    let err = rc_git_guard::actions::checkout_branch(fx.path(), &policy, "feature").unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "checkout 'feature': already used by worktree at '{}'",
            wt.display()
        )
    );
    assert_eq!(fx.git_output(&["symbolic-ref", "--short", "HEAD"]), "main");
    assert!(
        !fx.path().join("feature.txt").exists(),
        "nothing was written"
    );

    let err = rc_git_guard::actions::checkout_branch(&wt, &policy, "main").unwrap_err();
    assert!(
        err.to_string().contains("already used by worktree at"),
        "{err}"
    );
    let wt_head = fx.git_output(&[
        "-C",
        wt.to_str().unwrap(),
        "symbolic-ref",
        "--short",
        "HEAD",
    ]);
    assert_eq!(wt_head, "feature");
}

/// The pure planners the Changes tab and the ref sheets ask before acting. Moved from the
/// FFI's `guard_plans_carry_the_changes_tab_policy`.
#[test]
fn the_planners_carry_the_changes_tab_policy() {
    use rc_git_guard::actions::{
        plan_commit, plan_discard, plan_stash_apply, plan_stash_drop, plan_stash_pop,
        plan_stash_push, plan_tag_delete,
    };
    let policy = Policy::default();
    let confirm = |decision: Decision| match decision {
        Decision::Confirm(challenge) => challenge,
        Decision::Allow | Decision::Deny { .. } => panic!("expected Confirm, got {decision:?}"),
    };
    assert_eq!(plan_commit(&policy, 2), Decision::Allow);
    assert_eq!(plan_stash_apply(&policy), Decision::Allow);

    let discard = confirm(plan_discard(&policy, 3));
    assert_eq!(
        discard.typed_phrase(),
        None,
        "a plain confirm answers a discard"
    );
    assert_eq!(discard.risk(), Risk::Destructive);

    let push = confirm(plan_stash_push(&policy, 2));
    assert_eq!(push.typed_phrase(), None);

    let drop = confirm(plan_stash_drop(&policy, 1, "On main: wip"));
    assert_eq!(drop.typed_phrase(), Some("drop"));
    assert!(drop.backup_ref());
    assert!(
        drop.summary().contains("stash@{1} \"On main: wip\""),
        "the challenge names the entry: {}",
        drop.summary()
    );

    let pop = confirm(plan_stash_pop(&policy, 0, "On main: wip"));
    assert!(pop.backup_ref());
    assert!(
        pop.summary().contains("stash@{0} \"On main: wip\""),
        "{}",
        pop.summary()
    );

    let tag = confirm(plan_tag_delete(&policy, "v1.0"));
    assert_eq!(tag.typed_phrase(), Some("v1.0"));
    assert!(tag.backup_ref());
}

/// A pull request on `main` as a fresh read shows it: open, mergeable, clean, two checks.
#[expect(clippy::unwrap_used, reason = "test helper parses a fixed commit id")]
fn mergeable_pull_request() -> rc_git_guard::actions::github::PullRequestDetail {
    use rc_git_guard::actions::github::{
        Check, CheckList, CheckProgress, DetailFields, MergeMethods, MergeStateStatus, Mergeable,
        PullRequestDetail, PullRequestId, PullRequestState, PullRequestSummary, StatusState,
    };
    let status = |name: &str, state| Check {
        name: name.to_owned(),
        progress: CheckProgress::Status(state),
        description: None,
        required: None,
        details: None,
        actions: None,
    };
    PullRequestDetail {
        summary: PullRequestSummary {
            number: 12,
            title: "Add the thing".to_owned(),
            author: Some("mona".to_owned()),
            head_ref: "topic".to_owned(),
            head_oid: rc_git_core::Oid::from_hex(&"cd".repeat(20)).unwrap(),
            base_ref: "main".to_owned(),
            head_owner: Some("octo".to_owned()),
            cross_repository: false,
            draft: false,
            state: PullRequestState::Open,
            updated_at: None,
            checks: None,
            review: None,
        },
        id: PullRequestId::parse("PR_kwDOtwelve"),
        base_oid: None,
        body: String::new(),
        url: None,
        mergeable: Mergeable::Mergeable,
        merge_state: Some(MergeStateStatus::Clean),
        merge_methods: MergeMethods {
            merge: true,
            squash: false,
            rebase: true,
        },
        auto_merge_allowed: None,
        viewer: None,
        auto_merge: None,
        checks: CheckList {
            checks: vec![
                status("ci", StatusState::Success),
                status("deploy", StatusState::Pending),
            ],
            total: 2,
        },
        fields: DetailFields::Full,
    }
}

/// The merge row of `docs/safety.md`: a plain confirm, no backup, built only from a read that
/// nothing stands in the way of, naming what was read.
#[test]
fn a_pull_request_merge_is_a_plain_confirm_built_from_its_read() {
    use rc_git_guard::Action;
    use rc_git_guard::actions::github::{
        GitHubHost, MergeBlocker, MergeMethod, MergeStateStatus, Mergeable, PullRequestMerge,
        RepositoryTarget,
    };
    let target =
        RepositoryTarget::on_host(&GitHubHost::github_com(), "https://github.com/octo/rc-git")
            .unwrap();
    let detail = mergeable_pull_request();
    let merge = PullRequestMerge::new(&target, "octocat", &detail, MergeMethod::Rebase).unwrap();
    let Decision::Confirm(challenge) =
        Policy::default().evaluate(&Action::MergePullRequest(Box::new(merge)))
    else {
        panic!("a merge is confirmed");
    };
    assert_eq!(challenge.typed_phrase(), None, "no phrase to type");
    assert!(!challenge.backup_ref(), "nothing local to back up");
    assert_eq!(challenge.risk(), Risk::Recoverable);
    assert!(
        challenge.summary().ends_with(
            "\nRepository: octo/rc-git on github.com\nPull request: #12\n\
             Account: octocat@github.com\nBase: \u{201c}main\u{201d}\n\
             Head: cdcdcdc on \u{201c}topic\u{201d}\nMethod: a rebase merge\n\
             Checks: 1 of 2 checks passed, 1 pending\nTitle: \u{201c}Add the thing\u{201d}"
        ),
        "{}",
        challenge.summary()
    );

    let blocked = |change: fn(&mut rc_git_guard::actions::github::PullRequestDetail)| {
        let mut detail = mergeable_pull_request();
        change(&mut detail);
        PullRequestMerge::new(&target, "octocat", &detail, MergeMethod::Rebase).unwrap_err()
    };
    assert_eq!(blocked(|d| d.summary.draft = true), MergeBlocker::Draft);
    assert_eq!(
        blocked(|d| d.mergeable = Mergeable::Conflicting),
        MergeBlocker::Conflicting
    );
    assert_eq!(
        blocked(|d| d.merge_state = Some(MergeStateStatus::Blocked)),
        MergeBlocker::Blocked
    );
    assert_eq!(
        blocked(|d| d.merge_methods.rebase = false),
        MergeBlocker::MethodNotAllowed(MergeMethod::Rebase)
    );
    assert_eq!(blocked(|d| d.id = None), MergeBlocker::NoId);
}

/// The admin merge row of `docs/safety.md`: the pull request's number typed, no backup,
/// destructive, built only from a read branch protection blocks and that the account may merge
/// past, naming what it skips.
#[test]
fn an_admin_merge_needs_the_number_typed_and_a_blocked_read_the_account_may_merge_past() {
    use rc_git_guard::Action;
    use rc_git_guard::actions::github::{
        GitHubHost, MergeBlocker, MergeMethod, MergeStateStatus, PullRequestAdminMerge,
        RepositoryTarget, ReviewDecision, ViewerAbilities,
    };
    let target =
        RepositoryTarget::on_host(&GitHubHost::github_com(), "https://github.com/octo/rc-git")
            .unwrap();
    let mut detail = mergeable_pull_request();
    detail.merge_state = Some(MergeStateStatus::Blocked);
    detail.summary.review = Some(ReviewDecision::ReviewRequired);
    detail.viewer = Some(ViewerAbilities {
        can_merge_as_admin: true,
        ..ViewerAbilities::default()
    });
    let admin =
        PullRequestAdminMerge::new(&target, "octocat", &detail, MergeMethod::Rebase).unwrap();
    assert_eq!(
        admin.bypassed().review,
        Some(ReviewDecision::ReviewRequired)
    );
    assert_eq!(
        admin.bypassed().unreported,
        1,
        "deploy is pending and the host did not say whether it is required"
    );
    let action = Action::AdminMergePullRequest(Box::new(admin));
    let Decision::Confirm(challenge) = Policy::default().evaluate(&action) else {
        panic!("an admin merge is confirmed");
    };
    assert_eq!(challenge.typed_phrase(), Some("12"));
    assert!(!challenge.backup_ref());
    assert_eq!(challenge.risk(), Risk::Destructive);
    assert!(
        challenge.summary().starts_with(
            "Merge this pull request on GitHub past its branch protection, as an administrator."
        ) && challenge
            .summary()
            .contains("\nBase: \u{201c}main\u{201d}\nHead: cdcdcdc on \u{201c}topic\u{201d}\n"),
        "{}",
        challenge.summary()
    );
    assert!(
        challenge
            .summary()
            .contains("\nSkipped: a required review that has not approved it\n"),
        "{}",
        challenge.summary()
    );
    assert!(challenge.verify("12").is_some());
    assert!(challenge.verify("#12").is_none());
    assert!(challenge.verify("").is_none());

    let refused = |change: fn(&mut rc_git_guard::actions::github::PullRequestDetail)| {
        let mut detail = detail.clone();
        change(&mut detail);
        PullRequestAdminMerge::new(&target, "octocat", &detail, MergeMethod::Rebase).unwrap_err()
    };
    assert_eq!(
        refused(|d| d.merge_state = Some(MergeStateStatus::Clean)),
        MergeBlocker::NothingToBypass
    );
    assert_eq!(refused(|d| d.viewer = None), MergeBlocker::NoAdminRight);
    assert_eq!(refused(|d| d.summary.draft = true), MergeBlocker::Draft);
}

/// The auto-merge rows of `docs/safety.md`: a plain confirm each, no backup, built only from a
/// read that allows the change. Turning it on says GitHub merges later commits without asking.
#[test]
fn turning_auto_merge_on_or_off_is_a_plain_confirm_built_from_its_read() {
    use rc_git_guard::Action;
    use rc_git_guard::actions::github::{
        ActionBlocker, AutoMerge, AutoMergeDisable, AutoMergeEnable, GitHubHost, MergeMethod,
        MergeStateStatus, RepositoryTarget, ViewerAbilities,
    };
    let target =
        RepositoryTarget::on_host(&GitHubHost::github_com(), "https://github.com/octo/rc-git")
            .unwrap();
    let mut detail = mergeable_pull_request();
    detail.merge_state = Some(MergeStateStatus::Blocked);
    detail.auto_merge_allowed = Some(true);
    detail.viewer = Some(ViewerAbilities {
        can_enable_auto_merge: true,
        can_disable_auto_merge: true,
        ..ViewerAbilities::default()
    });
    let enable = AutoMergeEnable::new(&target, "octocat", &detail, MergeMethod::Rebase).unwrap();
    let mut on = detail.clone();
    on.auto_merge = Some(AutoMerge {
        method: Some(MergeMethod::Rebase),
        enabled_by: Some("mona".to_owned()),
    });
    let disable = AutoMergeDisable::new(&target, "octocat", &on).unwrap();
    for (action, says) in [
        (
            Action::EnableAutoMerge(Box::new(enable)),
            "\nMerges by itself: GitHub will merge this pull request by itself when its \
             requirements are met, including commits pushed to its head branch after this \
             confirmation; rc-git will not ask again\n",
        ),
        (
            Action::DisableAutoMerge(Box::new(disable)),
            "\nAuto-merge: on, as a rebase merge, turned on by \u{201c}mona\u{201d}\n",
        ),
    ] {
        let Decision::Confirm(challenge) = Policy::default().evaluate(&action) else {
            panic!("{action:?} is confirmed");
        };
        assert_eq!(challenge.typed_phrase(), None);
        assert!(!challenge.backup_ref());
        assert_eq!(challenge.risk(), Risk::Recoverable);
        assert!(
            challenge.summary().contains(says),
            "{}",
            challenge.summary()
        );
    }
    let mut clean = detail.clone();
    clean.merge_state = Some(MergeStateStatus::Clean);
    assert_eq!(
        AutoMergeEnable::new(&target, "octocat", &clean, MergeMethod::Rebase).unwrap_err(),
        ActionBlocker::CanMergeNow
    );
    assert_eq!(
        AutoMergeEnable::new(&target, "octocat", &on, MergeMethod::Rebase).unwrap_err(),
        ActionBlocker::AutoMergeOn
    );
    assert_eq!(
        AutoMergeDisable::new(&target, "octocat", &detail).unwrap_err(),
        ActionBlocker::AutoMergeOff
    );
}

/// The re-run row of `docs/safety.md`: a plain confirm, no backup, built only from a read with a
/// failed GitHub Actions run, naming each run it asks for.
#[test]
fn a_rerun_of_failed_jobs_is_a_plain_confirm_naming_its_runs() {
    use rc_git_guard::Action;
    use rc_git_guard::actions::github::{
        ActionBlocker, ActionsIds, Check, CheckConclusion, CheckProgress, CheckStatus, GitHubHost,
        PullRequestRerun, RepositoryTarget,
    };
    let target =
        RepositoryTarget::on_host(&GitHubHost::github_com(), "https://github.com/octo/rc-git")
            .unwrap();
    let mut detail = mergeable_pull_request();
    assert_eq!(
        PullRequestRerun::new(&target, "octocat", &detail).unwrap_err(),
        ActionBlocker::NoFailedRuns
    );
    detail.checks.checks.push(Check {
        name: "test".to_owned(),
        progress: CheckProgress::Run {
            status: CheckStatus::Completed,
            conclusion: Some(CheckConclusion::Failure),
        },
        description: None,
        required: None,
        details: None,
        actions: Some(ActionsIds {
            workflow_run: 31,
            job: 5,
        }),
    });
    let rerun = PullRequestRerun::new(&target, "octocat", &detail).unwrap();
    let Decision::Confirm(challenge) =
        Policy::default().evaluate(&Action::RerunFailedJobs(Box::new(rerun)))
    else {
        panic!("a re-run is confirmed");
    };
    assert_eq!(challenge.typed_phrase(), None);
    assert!(!challenge.backup_ref());
    assert_eq!(challenge.risk(), Risk::Recoverable);
    assert!(
        challenge
            .summary()
            .contains("\nRuns: 1 run: run 31 (\u{201c}test\u{201d})\n"),
        "{}",
        challenge.summary()
    );
}

#[test]
fn tag_deletion_demands_the_name_and_backs_up_the_tag_object() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let tag_object = fx.rev_parse("v1.0");
    let v1 = shown(&fx, "v1.0");

    let err = delete_tag(fx.path(), &policy, "v1.0", v1, None).unwrap_err();
    assert!(matches!(err, ActionError::NotConfirmed { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "delete tag 'v1.0': no typed confirmation was given; type 'v1.0' to confirm"
    );
    let blank = delete_tag(fx.path(), &policy, "v1.0", v1, Some("  ")).unwrap_err();
    assert_eq!(
        blank.to_string(),
        err.to_string(),
        "blank input counts as none"
    );

    let deleted = delete_tag(fx.path(), &policy, "v1.0", v1, Some(" v1.0 ")).unwrap();
    assert_eq!(
        deleted.backup_ref.as_deref(),
        Some("refs/rc-git/backup/tag/v1.0")
    );
    assert!(!refs(&fx).contains("refs/tags/v1.0"));
    assert_eq!(
        fx.rev_parse("refs/rc-git/backup/tag/v1.0"),
        tag_object,
        "an annotated tag's backup keeps the tag object, message and all"
    );

    let err = delete_tag(fx.path(), &policy, "nope", v1, Some("nope")).unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
}

#[test]
fn a_tag_moved_since_it_was_shown_is_not_deleted() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let before = shown(&fx, "v0.9");
    fx.git(&["tag", "-f", "v0.9", "feature"]);
    let moved = fx.rev_parse("v0.9");
    assert_ne!(moved, before.to_hex(), "the fixture must move the tag");

    let err = delete_tag(fx.path(), &policy, "v0.9", before, Some("v0.9")).unwrap_err();
    assert!(matches!(err, ActionError::StaleRepository { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "delete tag: the repository changed since the list was built; refresh and retry"
    );
    assert_eq!(fx.rev_parse("v0.9"), moved, "the moved tag is kept");
    assert!(!refs(&fx).contains("refs/rc-git/backup"));
}

#[test]
fn an_annotated_tag_is_judged_by_the_commit_it_peels_to() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let tag_object = rc_git_core::Oid::from_hex(&fx.rev_parse("v1.0")).unwrap();
    assert_ne!(tag_object, shown(&fx, "v1.0"), "v1.0 must be annotated");

    let err = delete_tag(fx.path(), &policy, "v1.0", tag_object, Some("v1.0")).unwrap_err();
    assert!(matches!(err, ActionError::StaleRepository { .. }), "{err}");
    delete_tag(fx.path(), &policy, "v1.0", shown(&fx, "v1.0"), Some("v1.0")).unwrap();
    assert!(!refs(&fx).contains("refs/tags/v1.0"));
}

#[test]
fn a_tag_nested_under_a_backed_up_name_still_backs_up() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let target = fx.rev_parse("v0.9");
    let deleted = delete_tag(fx.path(), &policy, "v0.9", shown(&fx, "v0.9"), Some("v0.9")).unwrap();
    assert_eq!(
        deleted.backup_ref.as_deref(),
        Some("refs/rc-git/backup/tag/v0.9")
    );

    fx.git(&["tag", "v0.9/rc", &target]);
    let deleted = delete_tag(
        fx.path(),
        &policy,
        "v0.9/rc",
        shown(&fx, "v0.9/rc"),
        Some("v0.9/rc"),
    )
    .unwrap();
    assert_eq!(
        deleted.backup_ref.as_deref(),
        Some("refs/rc-git/backup/tag/v0.9%2Frc")
    );
    assert!(!refs(&fx).contains("refs/tags/v0.9/rc"));
    assert_eq!(fx.rev_parse("refs/rc-git/backup/tag/v0.9%2Frc"), target);
}

/// The standard fixture with `core.ignorecase=true`, as git sets it on a case-insensitive
/// volume. Every case-variant name below must fail on either kind of volume.
fn ignore_case_fixture() -> Fixture {
    let fx = Fixture::standard();
    fx.git(&["config", "core.ignorecase", "true"]);
    fx
}

#[test]
fn a_case_variant_of_the_checked_out_branch_is_refused() {
    let fx = ignore_case_fixture();
    let main_tip = fx.rev_parse("main");
    let policy = Policy::default();

    assert!(delete_branch(fx.path(), &policy, "MAIN", None).is_err());
    assert!(delete_branch(fx.path(), &policy, "Main", Some("Main")).is_err());
    assert!(plan_branch_delete(fx.path(), &policy, "MAIN").is_err());
    assert_eq!(fx.rev_parse("main"), main_tip);
    assert_eq!(fx.rev_parse("HEAD"), main_tip, "HEAD still resolves");
    assert!(!refs(&fx).contains("refs/rc-git/backup"));
}

#[test]
fn a_case_variant_of_a_protected_branch_is_denied() {
    let fx = ignore_case_fixture();
    let policy = Policy::default();
    fx.git(&["checkout", "-q", "feature"]);
    fx.git(&["branch", "Release/2", "main"]);

    let err = delete_branch(fx.path(), &policy, "MAIN", Some("MAIN")).unwrap_err();
    assert!(err.to_string().contains("refs/heads/MAIN"), "{err}");
    assert!(refs(&fx).contains("refs/heads/main"));

    let err = delete_branch(fx.path(), &policy, "Release/2", Some("Release/2")).unwrap_err();
    assert!(matches!(err, ActionError::Denied { .. }), "{err}");
    assert!(refs(&fx).contains("refs/heads/Release/2"));
}

#[test]
fn a_case_variant_of_an_unmerged_branch_is_refused_without_a_backup() {
    let fx = ignore_case_fixture();
    let feature_tip = fx.rev_parse("feature");

    let err = delete_branch(fx.path(), &Policy::default(), "FEATURE", Some("FEATURE")).unwrap_err();
    assert!(
        err.to_string()
            .contains("'refs/heads/feature' differs only in case"),
        "{err}"
    );
    assert_eq!(fx.rev_parse("feature"), feature_tip);
    assert!(!refs(&fx).contains("refs/rc-git/backup"));
}

#[test]
fn a_case_variant_of_a_packed_ref_is_never_created_or_deleted() {
    let fx = ignore_case_fixture();
    let policy = Policy::default();
    fx.git(&["pack-refs", "--all"]);
    let main_tip = fx.rev_parse("main");
    let tag_object = fx.rev_parse("v1.0");

    let err = create_branch(fx.path(), &policy, "MAIN", "feature").unwrap_err();
    assert!(
        err.to_string().contains("refs/heads/main"),
        "names the existing spelling: {err}"
    );
    assert_eq!(fx.rev_parse("main"), main_tip);
    assert!(create_tag(fx.path(), &policy, "V1.0", "feature").is_err());
    assert!(delete_tag(fx.path(), &policy, "V1.0", shown(&fx, "v1.0"), Some("V1.0")).is_err());
    assert_eq!(fx.rev_parse("v1.0"), tag_object);
    assert!(!refs(&fx).contains("refs/heads/MAIN"));
    assert!(!refs(&fx).contains("refs/rc-git/backup"));
}

#[test]
fn a_branch_with_a_case_variant_sibling_is_refused() {
    let fx = ignore_case_fixture();
    fx.git(&["branch", "topic", "main~1"]);
    fx.git(&["pack-refs", "--all"]);
    fx.git(&["update-ref", "refs/heads/Topic", "feature"]);
    let listed = refs(&fx);
    assert!(listed.contains("refs/heads/Topic") && listed.contains("refs/heads/topic"));

    let err = delete_branch(fx.path(), &Policy::default(), "Topic", Some("Topic")).unwrap_err();
    assert!(err.to_string().contains("differs only in case"), "{err}");
    assert_eq!(refs(&fx), listed, "both refs survive");
}

mod commit_and_discard {
    #![expect(
        clippy::disallowed_methods,
        reason = "tests arrange the fixture's working tree; only the guard mutates the repository"
    )]

    use rc_git_core::{BlobDiff, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT, Oid, Repository};
    use rc_git_guard::Policy;
    use rc_git_guard::actions::{
        ActionError, Author, CommitRequest, DiscardKind, DiscardOutcome, FileSelection, HunkStamp,
        Identity, commit, configured_identity, discard_files, discard_hunks,
    };
    use rc_git_testkit::Fixture;

    use super::{explicit, identity, porcelain};

    #[expect(
        clippy::unwrap_used,
        reason = "test helper panics on a malformed fixture id"
    )]
    fn commit_id(hex: &str) -> Oid {
        Oid::from_hex(hex).unwrap()
    }

    fn whole(path: &str) -> FileSelection {
        FileSelection::Whole {
            path: path.to_owned(),
        }
    }

    /// The stamps of the current worktree diff of `path` against HEAD, in file order.
    #[expect(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "test helper panics on fixture failure"
    )]
    fn stamps(fx: &Fixture, path: &str) -> Vec<HunkStamp> {
        let repo = Repository::open(fx.path()).unwrap();
        let old = repo
            .resolve(&format!("HEAD:{path}"))
            .expect("path exists in HEAD");
        let diff = repo
            .diff_worktree_file(Some(old), path, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)
            .unwrap();
        let BlobDiff::Text { hunks } = diff else {
            panic!("expected a text diff");
        };
        hunks.iter().map(HunkStamp::from).collect()
    }

    fn ls_tree_mode(fx: &Fixture, path: &str) -> String {
        fx.git_output(&["ls-tree", "HEAD", path])
            .split(' ')
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    #[expect(clippy::unwrap_used, reason = "test helper panics on a failed commit")]
    fn commit_whole(fx: &Fixture, paths: &[&str]) {
        let head = fx.rev_parse("HEAD");
        let selections: Vec<FileSelection> = paths.iter().map(|path| whole(path)).collect();
        commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "modes",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &selections,
            },
            None,
        )
        .unwrap();
    }

    #[test]
    fn only_the_owner_exec_bit_makes_a_committed_file_executable() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"edited\n");
        fx.set_mode("alpha.txt", 0o654);

        commit_whole(&fx, &["alpha.txt"]);
        assert_eq!(ls_tree_mode(&fx, "alpha.txt"), "100644");
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn the_owner_exec_bit_alone_makes_a_committed_file_executable() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"edited\n");
        fx.set_mode("alpha.txt", 0o744);

        commit_whole(&fx, &["alpha.txt"]);
        assert_eq!(ls_tree_mode(&fx, "alpha.txt"), "100755");
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn with_file_mode_off_a_commit_keeps_the_recorded_mode_whatever_the_disk_says() {
        let fx = Fixture::standard();
        fx.git(&["config", "core.fileMode", "false"]);
        fx.write("alpha.txt", b"edited\n");
        fx.set_mode("alpha.txt", 0o755);
        fx.write("fresh.txt", b"new\n");
        fx.set_mode("fresh.txt", 0o755);

        commit_whole(&fx, &["alpha.txt", "fresh.txt"]);
        assert_eq!(ls_tree_mode(&fx, "alpha.txt"), "100644");
        assert_eq!(ls_tree_mode(&fx, "fresh.txt"), "100644");
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn a_new_symlink_commits_as_a_link_to_its_target() {
        let fx = Fixture::standard();
        fx.symlink("alpha.txt", "alpha-link");

        commit_whole(&fx, &["alpha-link"]);
        assert_eq!(ls_tree_mode(&fx, "alpha-link"), "120000");
        assert_eq!(
            fx.git_output(&["cat-file", "-p", "HEAD:alpha-link"]),
            "alpha.txt"
        );
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn committing_a_directory_by_its_own_path_is_refused() {
        let fx = Fixture::standard();
        fx.write("fresh/one.txt", b"one\n");
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "a folder",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("fresh")],
            },
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "commit", name, .. } if name == "fresh"),
            "{err}"
        );
        assert!(err.to_string().contains("is a directory"), "{err}");
        assert_eq!(fx.rev_parse("HEAD"), head, "nothing moved");
    }

    #[test]
    fn committing_a_file_that_became_a_directory_names_the_type_change() {
        let fx = Fixture::standard();
        fx.remove("alpha.txt");
        fx.write("alpha.txt/b.txt", b"inside\n");
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "file to folder",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("alpha.txt")],
            },
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "commit", name, .. } if name == "alpha.txt"),
            "{err}"
        );
        let message = err.to_string();
        assert!(
            message.contains("was a file in HEAD and is now a directory")
                && message.contains("select the files under it"),
            "{message}"
        );
        assert_eq!(fx.rev_parse("HEAD"), head, "nothing moved");
    }

    #[test]
    fn a_path_under_a_submodule_reads_as_absent_from_head() {
        let fx = Fixture::standard();
        let gitlink = "160000,1111111111111111111111111111111111111111,sub";
        fx.git(&["update-index", "--add", "--cacheinfo", gitlink]);
        fx.git(&["commit", "-q", "-m", "add submodule"]);
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "under a gitlink",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("sub/x.c")],
            },
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("does not exist on disk or in HEAD"),
            "the submodule's commit is never looked up here: {err}"
        );
        assert_eq!(fx.rev_parse("HEAD"), head);
    }

    #[test]
    fn a_file_inside_a_checked_out_submodule_is_refused_and_the_gitlink_kept() {
        let upstream = Fixture::standard();
        let fx = Fixture::standard();
        let source = upstream.path().to_string_lossy().into_owned();
        fx.git(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &source,
            "sub",
        ]);
        fx.git(&["commit", "-q", "-m", "add submodule"]);
        fx.write("sub/alpha.txt", b"edited inside the submodule\n");
        fx.write("alpha.txt", b"edited in the superproject\n");
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "into a submodule",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("alpha.txt"), whole("sub/alpha.txt")],
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{err}");
        assert!(
            err.to_string().contains("'sub/alpha.txt'")
                && err.to_string().contains("inside submodule 'sub'"),
            "{err}"
        );
        assert_eq!(fx.rev_parse("HEAD"), head);
        assert_eq!(ls_tree_mode(&fx, "sub"), "160000");
        assert_eq!(
            fx.git_output(&["diff", "--cached", "--name-only"]),
            "",
            "the index is untouched"
        );
    }

    #[test]
    fn a_mis_cased_selection_is_refused_and_head_is_unchanged() {
        let fx = Fixture::standard();
        fx.git(&["config", "core.ignorecase", "true"]);
        if !fx.path().join("ALPHA.TXT").exists() {
            return; // A case-sensitive volume keeps `ALPHA.TXT` and `alpha.txt` apart.
        }
        fx.write("alpha.txt", b"changed\n");
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "case",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("ALPHA.TXT")],
            },
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("'ALPHA.TXT' is spelled 'alpha.txt' on disk"),
            "{err}"
        );
        assert_eq!(fx.rev_parse("HEAD"), head);
        assert_eq!(porcelain(&fx), " M alpha.txt");

        fx.git(&["mv", "docs", "docs-tmp"]);
        fx.git(&["mv", "docs-tmp", "Docs"]);
        fx.git(&["commit", "-qm", "rename docs"]);
        let head = fx.rev_parse("HEAD");
        fx.write("Docs/guide.md", b"edited\n");
        let err = commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "case",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("docs/guide.md")],
            },
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("'docs' is spelled 'Docs'"),
            "{err}"
        );
        assert_eq!(fx.rev_parse("HEAD"), head);
    }

    #[test]
    fn a_commit_reaches_heads_reflog_so_head_at_one_is_the_previous_commit() {
        let fx = Fixture::standard();
        let before = fx.rev_parse("HEAD");
        fx.write("alpha.txt", b"changed\n");

        commit_whole(&fx, &["alpha.txt"]);
        assert_eq!(
            fx.git_output(&["log", "-g", "-1", "--format=%gs", "HEAD"]),
            "rc-git: commit"
        );
        assert_eq!(fx.rev_parse("HEAD@{0}"), fx.rev_parse("HEAD"));
        assert_eq!(
            fx.rev_parse("HEAD@{1}"),
            before,
            "`git reset --hard HEAD@{{1}}` must undo exactly this commit"
        );
    }

    #[test]
    fn an_unwritable_head_reflog_refuses_a_commit_before_writing() {
        let fx = Fixture::standard();
        let head = fx.rev_parse("HEAD");
        fx.write("alpha.txt", b"changed\n");
        fx.set_mode(".git/logs/HEAD", 0o444);

        let err = commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "refused",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("alpha.txt")],
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{err}");
        assert!(err.to_string().contains("logs/HEAD"), "{err}");
        assert_eq!(fx.rev_parse("main"), head, "the branch must not move");
    }

    #[test]
    fn a_commit_records_the_selection_and_leaves_git_status_clean() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"changed\n");
        fx.write("brand-new.txt", b"new\n");
        std::fs::remove_file(fx.path().join("Zeta.txt")).unwrap();
        fx.write("left-alone.txt", b"stays dirty\n");
        let head = fx.rev_parse("HEAD");

        let done = commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "  Commit from rc-git  ",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[
                    whole("alpha.txt"),
                    whole("brand-new.txt"),
                    whole("Zeta.txt"),
                ],
            },
            None,
        )
        .unwrap();
        assert_eq!(done.branch, "main");
        assert_eq!(done.oid.to_hex(), fx.rev_parse("HEAD"));

        let log = fx.git_output(&["log", "-1", "--format=%an|%ae|%s"]);
        assert_eq!(log, "Test User|test@example.com|Commit from rc-git");
        assert_eq!(
            fx.git_output(&["show", "HEAD:alpha.txt"]),
            "changed",
            "git_output trims the trailing newline"
        );
        assert_eq!(fx.git_output(&["show", "HEAD:brand-new.txt"]), "new");
        assert!(
            !fx.git_output(&["ls-tree", "-r", "HEAD"])
                .contains("Zeta.txt")
        );
        assert_eq!(
            porcelain(&fx),
            "?? left-alone.txt",
            "committed paths are clean in terminal git; the unselected file stays untracked"
        );
    }

    /// Terminal git agrees with the commit: `git write-tree` builds a tree from the index (it
    /// refuses `d` beside `d/x`), and `git status` shows nothing left to commit.
    fn assert_index_matches_head(fx: &Fixture, case: &str) {
        assert_eq!(
            fx.git_output(&["write-tree"]),
            fx.rev_parse("HEAD^{tree}"),
            "{case}: the index holds exactly the committed tree"
        );
        assert_eq!(porcelain(fx), "", "{case}: the committed paths are clean");
    }

    #[test]
    fn a_directory_replaced_by_a_file_commits_with_a_consistent_index() {
        let cases: [&[&str]; 3] = [
            &["src"],
            &["src", "src/lib.rs", "src/main.rs"],
            &["src/main.rs", "src/lib.rs", "src"],
        ];
        for selection in cases {
            let fx = Fixture::standard();
            fx.remove("src");
            fx.write("src", b"now a file\n");
            commit_whole(&fx, selection);
            let case = selection.join(" ");
            assert_eq!(fx.git_output(&["show", "HEAD:src"]), "now a file", "{case}");
            assert_index_matches_head(&fx, &case);
        }
    }

    #[test]
    fn a_file_replaced_by_a_directory_commits_with_a_consistent_index() {
        let fx = Fixture::standard();
        fx.remove("alpha.txt");
        fx.write("alpha.txt/inner", b"inner\n");
        commit_whole(&fx, &["alpha.txt/inner"]);
        assert_eq!(fx.git_output(&["show", "HEAD:alpha.txt/inner"]), "inner");
        assert_index_matches_head(&fx, "alpha.txt/inner");
    }

    #[test]
    fn a_deleted_directory_committed_by_its_own_path_leaves_no_index_entries_under_it() {
        let fx = Fixture::standard();
        fx.remove("src");
        commit_whole(&fx, &["src"]);
        assert_eq!(fx.git_output(&["ls-files", "src"]), "");
        assert_index_matches_head(&fx, "src");
    }

    #[test]
    fn a_nested_edit_rewrites_its_folder_and_keeps_the_sibling_tree() {
        let fx = Fixture::standard();
        fx.write("src/lib.rs", b"pub fn two() -> u32 {\n    2\n}\n");
        commit_whole(&fx, &["src/lib.rs"]);
        assert_eq!(
            fx.git_output(&["show", "HEAD:src/lib.rs"]),
            "pub fn two() -> u32 {\n    2\n}"
        );
        assert_eq!(
            fx.git_output(&["show", "HEAD:src/main.rs"]),
            fx.git_output(&["show", "HEAD~1:src/main.rs"]),
            "the untouched file in the rewritten folder is kept"
        );
        assert_eq!(
            fx.rev_parse("HEAD:docs"),
            fx.rev_parse("HEAD~1:docs"),
            "an untouched sibling folder keeps its tree id"
        );
        assert_index_matches_head(&fx, "src/lib.rs");
    }

    #[test]
    fn removing_the_last_file_of_a_folder_drops_the_folder() {
        let fx = Fixture::standard();
        fx.remove("docs/guide.md");
        commit_whole(&fx, &["docs/guide.md"]);
        assert_eq!(fx.git_output(&["ls-tree", "HEAD", "docs"]), "");
        fx.git(&["fsck", "--strict", "--no-dangling"]);
        assert_index_matches_head(&fx, "docs/guide.md");
    }

    #[test]
    fn overlapping_and_duplicate_selections_are_refused_and_head_is_unchanged() {
        let fx = Fixture::standard();
        fx.remove("src");
        fx.write("alpha.txt", b"changed\n");
        let head = fx.rev_parse("HEAD");
        let cases: [&[&str]; 3] = [
            &["src", "src/lib.rs"],
            &["src/lib.rs", "src"],
            &["alpha.txt", "alpha.txt"],
        ];
        for selection in cases {
            let case = selection.join(" ");
            let selections: Vec<FileSelection> = selection.iter().map(|path| whole(path)).collect();
            let err = commit(
                fx.path(),
                &Policy::default(),
                CommitRequest {
                    message: "overlap",
                    author: explicit(),
                    expected_head: Some(commit_id(&head)),
                    selections: &selections,
                },
                None,
            )
            .unwrap_err();
            assert!(
                matches!(
                    &err,
                    ActionError::Git {
                        op: "build tree",
                        ..
                    }
                ),
                "{case}: {err}"
            );
            assert!(err.to_string().contains("overlap"), "{case}: {err}");
            assert_eq!(fx.rev_parse("HEAD"), head, "{case}: nothing moved");
        }
    }

    #[test]
    fn a_held_index_lock_refuses_the_commit_before_the_branch_moves() {
        let fx = Fixture::standard();
        let lock = fx.path().join(".git/index.lock");
        fx.write("alpha.txt", b"changed\n");
        fx.write(".git/index.lock", b"");
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "locked",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("alpha.txt")],
            },
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "commit", name, .. } if name == "index"),
            "{err}"
        );
        assert!(
            err.to_string()
                .contains("the index is locked by another git process; retry"),
            "{err}"
        );
        assert!(lock.exists(), "another process's lock is never removed");
        fx.remove(".git/index.lock");
        assert_eq!(fx.rev_parse("HEAD"), head, "the branch did not move");
        assert_eq!(
            porcelain(&fx),
            " M alpha.txt",
            "the change is still uncommitted"
        );

        let done = commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "retried",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("alpha.txt")],
            },
            None,
        )
        .unwrap();
        assert!(done.index_refreshed);
        assert!(!lock.exists(), "the commit releases the lock it took");
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn a_hunk_commit_takes_only_the_selected_hunk() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("hunky.txt", b"l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n");
        fx.commit("Add hunky", "2024-01-05T00:00:00+0000");
        fx.write(
            "hunky.txt",
            b"l1\nCHANGED\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nAPPENDED\n",
        );
        // Backdated to 2020-01-01: an mtime older than the index write keeps git from rehashing
        // the file as racily clean, so `git diff` below trusts the stat the commit recorded.
        let backdated = std::time::UNIX_EPOCH + std::time::Duration::from_hours(438_288);
        #[expect(
            clippy::disallowed_methods,
            reason = "the test backdates its own fixture file"
        )]
        std::fs::File::open(fx.path().join("hunky.txt"))
            .unwrap()
            .set_modified(backdated)
            .unwrap();

        let found = stamps(&fx, "hunky.txt");
        assert_eq!(found.len(), 2, "two separated edits make two hunks");
        let head = fx.rev_parse("HEAD");
        commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "Take the first hunk",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[FileSelection::Hunks {
                    path: "hunky.txt".to_owned(),
                    hunks: vec![found[0]],
                }],
            },
            None,
        )
        .unwrap();
        assert_eq!(
            fx.git_output(&["show", "HEAD:hunky.txt"]),
            "l1\nCHANGED\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10"
        );
        assert_eq!(
            std::fs::read(fx.path().join("hunky.txt")).unwrap(),
            b"l1\nCHANGED\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nAPPENDED\n",
            "the working tree is untouched"
        );
        assert_eq!(
            fx.git_output(&["diff", "--name-only"]),
            "hunky.txt",
            "the unselected hunk stays as an unstaged change"
        );
        assert_eq!(
            fx.git_output(&["diff", "--cached", "--name-only"]),
            "",
            "nothing is left staged"
        );
    }

    #[test]
    fn a_first_commit_on_an_unborn_branch_creates_it() {
        let fx = Fixture::empty_dir();
        fx.git(&["init", "-q", "-b", "main"]);
        fx.write("first.txt", b"first\n");
        let policy = Policy::default();

        let done = commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "First commit",
                author: explicit(),
                expected_head: None,
                selections: &[whole("first.txt")],
            },
            None,
        )
        .unwrap();
        assert_eq!(done.branch, "main");
        assert_eq!(fx.git_output(&["log", "--format=%s"]), "First commit");
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn an_invalid_identity_is_refused_before_any_object_is_written() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"changed\n");
        let head = fx.rev_parse("HEAD");
        let objects = fx.git_output(&["count-objects", "-v"]);
        let bad = |name: &str, email: &str| Identity {
            name: name.to_owned(),
            email: email.to_owned(),
        };

        for (author, field, value) in [
            (Some(bad("A", "b> <c")), "email", "b> <c"),
            (Some(bad("  ", "a@b")), "name", "  "),
            (Some(bad("A\nB", "a@b")), "name", "A\nB"),
            (None, "email", ""),
        ] {
            let author = match author {
                Some(identity) => Author {
                    explicit: Some(identity),
                    fallback: None,
                },
                None => Author {
                    explicit: None,
                    fallback: Some(bad("A", "")),
                },
            };
            let err = commit(
                fx.path(),
                &policy,
                CommitRequest {
                    message: "msg",
                    author,
                    expected_head: Some(commit_id(&head)),
                    selections: &[whole("alpha.txt")],
                },
                None,
            )
            .unwrap_err();
            assert!(
                matches!(
                    &err,
                    ActionError::InvalidIdentity { op: "commit", field: f, value: v, .. }
                        if *f == field && v == value
                ),
                "{err}"
            );
        }
        assert_eq!(fx.rev_parse("HEAD"), head);
        assert_eq!(
            fx.git_output(&["count-objects", "-v"]),
            objects,
            "a refused identity writes no object"
        );
    }

    #[test]
    fn stale_head_detached_head_and_missing_identity_are_refused() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"changed\n");
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "msg",
                author: explicit(),
                expected_head: Some(commit_id(&fx.rev_parse("HEAD~1"))),
                selections: &[whole("alpha.txt")],
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::StaleRepository { .. }), "{err}");

        let err = commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "msg",
                author: Author::default(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("alpha.txt")],
            },
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, ActionError::MissingIdentity { op: "commit" }),
            "{err}"
        );
        assert!(
            err.to_string().starts_with("commit: no author identity"),
            "{err}"
        );
        assert!(
            err.to_string()
                .contains("reads only this repository's .git/config (not ~/.gitconfig)"),
            "{err}"
        );

        let err = commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "   ",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("alpha.txt")],
            },
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("message is empty"), "{err}");

        fx.git(&["checkout", "-q", "--detach", "HEAD"]);
        let err = commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "msg",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("alpha.txt")],
            },
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, ActionError::DetachedHead { op: "commit" }),
            "{err}"
        );
        assert_eq!(fx.rev_parse("main"), head, "nothing moved");
    }

    #[test]
    fn empty_unchanged_missing_and_directory_selections_are_refused() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        let head = fx.rev_parse("HEAD");
        let cases: [(&[FileSelection], &str, &str); 4] = [
            (&[], "HEAD", "nothing selected to commit"),
            (
                &[whole("README.md")],
                "HEAD",
                "the selection matches HEAD; nothing to commit",
            ),
            (
                &[whole("nope.txt")],
                "nope.txt",
                "does not exist on disk or in HEAD",
            ),
            (
                &[whole("src")],
                "src",
                "is a directory; select files instead",
            ),
        ];

        for (selections, expected_name, message) in cases {
            let err = commit(
                fx.path(),
                &policy,
                CommitRequest {
                    message: "msg",
                    author: explicit(),
                    expected_head: Some(commit_id(&head)),
                    selections,
                },
                None,
            )
            .unwrap_err();
            assert!(
                matches!(&err, ActionError::Git { op: "commit", name, .. } if name == expected_name),
                "{err}"
            );
            assert!(err.to_string().contains(message), "{err}");
            assert_eq!(fx.rev_parse("HEAD"), head, "nothing moved: {message}");
        }

        let err = discard_files(fx.path(), &policy, &["src".to_owned()], None).unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "discard", name, .. } if name == "src"),
            "{err}"
        );
        assert!(
            err.to_string()
                .contains("is a directory; select files instead"),
            "{err}"
        );
        assert_eq!(porcelain(&fx), "", "nothing was written");
    }

    #[test]
    fn discard_in_a_bare_repository_names_discard() {
        let fx = Fixture::standard();
        let (_holder, bare) = fx.bare_copy();
        let err =
            discard_files(&bare, &Policy::default(), &["alpha.txt".to_owned()], None).unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "discard", .. }),
            "{err}"
        );
        assert!(err.to_string().starts_with("discard "), "{err}");
        assert!(err.to_string().contains("bare"), "{err}");
    }

    #[test]
    fn repo_local_config_supplies_the_identity() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.git(&["config", "user.name", "Local Config"]);
        fx.git(&["config", "user.email", "local@example.com"]);
        fx.write("alpha.txt", b"changed\n");
        let head = fx.rev_parse("HEAD");

        commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "Identity from config",
                author: Author::default(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("alpha.txt")],
            },
            None,
        )
        .unwrap();
        assert_eq!(
            fx.git_output(&["log", "-1", "--format=%an|%ae"]),
            "Local Config|local@example.com"
        );
    }

    fn saved() -> Identity {
        Identity {
            name: "Saved Fallback".to_owned(),
            email: "saved@example.com".to_owned(),
        }
    }

    fn commit_alpha(fx: &Fixture, author: Author) -> Result<(), ActionError> {
        fx.write("alpha.txt", b"changed\n");
        let head = fx.rev_parse("HEAD");
        commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "Who wrote this",
                author,
                expected_head: Some(commit_id(&head)),
                selections: &[whole("alpha.txt")],
            },
            None,
        )
        .map(drop)
    }

    #[test]
    fn repo_config_beats_the_fallback() {
        let fx = Fixture::standard();
        fx.git(&["config", "user.name", "Repo Owner"]);
        fx.git(&["config", "user.email", "repo@example.com"]);

        let author = Author {
            explicit: None,
            fallback: Some(saved()),
        };
        commit_alpha(&fx, author).unwrap();
        assert_eq!(
            fx.git_output(&["log", "-1", "--format=%an|%ae|%cn|%ce"]),
            "Repo Owner|repo@example.com|Repo Owner|repo@example.com"
        );
    }

    #[test]
    fn the_fallback_fills_in_only_when_the_config_sets_no_identity() {
        let fx = Fixture::standard();
        assert_eq!(configured_identity(fx.path()).unwrap(), None);
        fx.git(&["config", "user.name", "Name Only"]);
        assert_eq!(
            configured_identity(fx.path()).unwrap(),
            None,
            "a name without an email is no identity"
        );

        let author = Author {
            explicit: None,
            fallback: Some(saved()),
        };
        commit_alpha(&fx, author).unwrap();
        assert_eq!(
            fx.git_output(&["log", "-1", "--format=%an|%ae|%cn|%ce"]),
            "Saved Fallback|saved@example.com|Saved Fallback|saved@example.com"
        );
    }

    #[test]
    fn an_explicit_identity_beats_the_repo_config() {
        let fx = Fixture::standard();
        fx.git(&["config", "user.name", "Repo Owner"]);
        fx.git(&["config", "user.email", "repo@example.com"]);
        assert_eq!(
            configured_identity(fx.path()).unwrap(),
            Some(Identity {
                name: "Repo Owner".to_owned(),
                email: "repo@example.com".to_owned(),
            })
        );

        let author = Author {
            explicit: Some(identity()),
            fallback: Some(saved()),
        };
        commit_alpha(&fx, author).unwrap();
        assert_eq!(
            fx.git_output(&["log", "-1", "--format=%ae"]),
            "test@example.com"
        );
    }

    #[test]
    fn configured_identity_names_a_path_that_is_no_repository() {
        let fx = Fixture::standard();
        let err = configured_identity(&fx.path().join("missing")).unwrap_err();
        assert!(err.to_string().contains("read commit identity"), "{err}");
    }

    #[test]
    fn a_mismatched_stamp_refuses_the_commit() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"changed\n");
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "msg",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[FileSelection::Hunks {
                    path: "alpha.txt".to_owned(),
                    hunks: vec![HunkStamp {
                        old_start: 99,
                        old_lines: 1,
                        new_start: 99,
                        new_lines: 1,
                    }],
                }],
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::HunkMismatch { .. }), "{err}");
        assert_eq!(fx.rev_parse("HEAD"), head, "nothing moved");
    }

    #[test]
    fn a_hunk_commit_of_a_file_replaced_by_a_symlink_is_refused() {
        let fx = Fixture::standard();
        let outside = Fixture::empty_dir();
        outside.write("outside.txt", b"SECRET-LINE-1\nline 2\n");
        let target = outside.path().join("outside.txt");
        fx.remove("alpha.txt");
        fx.symlink(target.to_str().unwrap(), "alpha.txt");
        let found = stamps(&fx, "alpha.txt");
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "first hunk",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[FileSelection::Hunks {
                    path: "alpha.txt".to_owned(),
                    hunks: found,
                }],
            },
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&err, ActionError::HunkMismatch { path } if path == "alpha.txt"),
            "{err}"
        );
        assert_eq!(fx.rev_parse("HEAD"), head, "nothing moved");
        assert!(
            !fx.git_output(&["cat-file", "-p", "HEAD:alpha.txt"])
                .contains("SECRET"),
            "the link target's bytes never reach a commit"
        );
    }

    #[test]
    fn an_empty_hunk_list_refuses_the_commit_and_names_the_path() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"changed\n");
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "msg",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[FileSelection::Hunks {
                    path: "alpha.txt".to_owned(),
                    hunks: vec![],
                }],
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::NoHunksSelected { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "commit: no hunks selected for 'alpha.txt'; select a hunk or the whole file"
        );
        assert_eq!(fx.rev_parse("HEAD"), head, "nothing moved");
    }

    #[test]
    fn discard_restores_tracked_files_and_deletes_untracked() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"dirty\n");
        fx.write("untracked.txt", b"junk\n");

        let outcomes = discard_files(
            fx.path(),
            &policy,
            &["alpha.txt".to_owned(), "untracked.txt".to_owned()],
            None,
        )
        .unwrap();
        assert_eq!(
            outcomes,
            [
                DiscardOutcome {
                    path: "alpha.txt".to_owned(),
                    kind: DiscardKind::Restored,
                },
                DiscardOutcome {
                    path: "untracked.txt".to_owned(),
                    kind: DiscardKind::Deleted,
                },
            ]
        );
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n"
        );
        assert!(!fx.path().join("untracked.txt").exists());
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn discard_also_unstages_a_staged_change() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"staged\n");
        fx.write("fresh.txt", b"new and staged\n");
        fx.write("README.md", b"staged elsewhere\n");
        fx.git(&["add", "alpha.txt", "fresh.txt", "README.md"]);

        discard_files(
            fx.path(),
            &Policy::default(),
            &["alpha.txt".to_owned(), "fresh.txt".to_owned()],
            None,
        )
        .unwrap();
        assert_eq!(
            porcelain(&fx),
            "M  README.md",
            "only the other path stays staged"
        );
        assert_eq!(
            fx.git_output(&["rev-parse", ":alpha.txt"]),
            fx.rev_parse("HEAD:alpha.txt"),
            "the index entry is HEAD's blob again"
        );
        assert_eq!(fx.git_output(&["ls-files", "--", "fresh.txt"]), "");
    }

    #[test]
    fn discard_refuses_a_tracked_directory_before_restoring_earlier_files() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"precious\n");

        let err = discard_files(
            fx.path(),
            &Policy::default(),
            &["alpha.txt".to_owned(), "docs".to_owned()],
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "discard", name, .. } if name == "docs"),
            "{err}"
        );
        assert!(err.to_string().contains("is a directory"), "{err}");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"precious\n",
            "the edit survives a refused discard"
        );
    }

    #[test]
    fn discard_refuses_a_submodule_before_restoring_earlier_files() {
        let fx = Fixture::standard();
        let gitlink = "160000,1111111111111111111111111111111111111111,sub";
        fx.git(&["update-index", "--add", "--cacheinfo", gitlink]);
        fx.git(&["commit", "-q", "-m", "add submodule"]);
        fx.write("alpha.txt", b"precious\n");

        let err = discard_files(
            fx.path(),
            &Policy::default(),
            &["alpha.txt".to_owned(), "sub".to_owned()],
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "discard", name, .. } if name == "sub"),
            "{err}"
        );
        assert!(err.to_string().contains("is a submodule"), "{err}");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"precious\n",
            "the edit survives a refused discard"
        );
    }

    #[test]
    fn discard_refuses_an_untracked_directory_listed_last_before_any_write() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"precious\n");
        fx.write("notes.txt", b"untracked\n");
        fx.write("newdir/inside.txt", b"untracked\n");

        let err = discard_files(
            fx.path(),
            &Policy::default(),
            &[
                "notes.txt".to_owned(),
                "alpha.txt".to_owned(),
                "newdir".to_owned(),
            ],
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "discard", name, .. } if name == "newdir"),
            "{err}"
        );
        assert!(err.to_string().contains("is a directory"), "{err}");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"precious\n"
        );
        assert!(
            fx.path().join("notes.txt").exists(),
            "notes.txt was deleted"
        );
        assert!(fx.path().join("newdir/inside.txt").exists());
    }

    #[test]
    fn discard_refuses_a_head_on_a_missing_commit_instead_of_deleting() {
        let fx = Fixture::standard();
        fx.write(
            ".git/refs/heads/main",
            format!("{}\n", "1".repeat(40)).as_bytes(),
        );
        fx.write("alpha.txt", b"edited\n");

        let err = discard_files(
            fx.path(),
            &Policy::default(),
            &["alpha.txt".to_owned()],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{err}");
        assert!(err.to_string().contains("HEAD"), "{err}");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"edited\n",
            "the edit survives a refused discard"
        );
    }

    #[test]
    fn discarding_an_untracked_file_removes_the_folders_it_leaves_empty() {
        let fx = Fixture::standard();
        fx.write("newdir/sub/a", b"junk\n");
        fx.write("newdir/keep", b"stays\n");

        discard_files(
            fx.path(),
            &Policy::default(),
            &["newdir/sub/a".to_owned()],
            None,
        )
        .unwrap();

        assert!(!fx.path().join("newdir/sub").exists());
        assert!(
            fx.path().join("newdir/keep").exists(),
            "pruning stops at the first folder that still holds something"
        );
    }

    #[test]
    fn discard_restores_a_symlink_and_the_exec_bit() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("tool.sh", b"#!/bin/sh\n");
        fx.set_mode("tool.sh", 0o755);
        fx.commit("Add tool", "2024-01-05T00:00:00+0000");
        fx.set_mode("tool.sh", 0o644);
        std::fs::remove_file(fx.path().join("link")).unwrap();
        fx.write("link", b"now a plain file\n");

        discard_files(
            fx.path(),
            &policy,
            &["link".to_owned(), "tool.sh".to_owned()],
            None,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_link(fx.path().join("link")).unwrap().to_str(),
            Some("README.md")
        );
        assert_eq!(file_mode(&fx, "tool.sh"), 0o755);
        assert_eq!(porcelain(&fx), "");
    }

    #[expect(clippy::unwrap_used, reason = "test helper panics on fixture failure")]
    pub(super) fn file_mode(fx: &Fixture, path: &str) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(fx.path().join(path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn discarding_a_hunk_keeps_the_exec_bit() {
        let fx = Fixture::standard();
        fx.write("hunky.txt", b"l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n");
        fx.set_mode("hunky.txt", 0o755);
        fx.commit("Add hunky", "2024-01-05T00:00:00+0000");
        fx.write(
            "hunky.txt",
            b"l1\nCHANGED\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nAPPENDED\n",
        );
        fx.set_mode("hunky.txt", 0o755);

        let found = stamps(&fx, "hunky.txt");
        assert_eq!(found.len(), 2);
        discard_hunks(
            fx.path(),
            &Policy::default(),
            "hunky.txt",
            &[found[0]],
            None,
        )
        .unwrap();
        assert_eq!(file_mode(&fx, "hunky.txt") & 0o111, 0o111);
        assert!(
            fx.git_output(&["diff", "--raw", "--", "hunky.txt"])
                .starts_with(":100755 100755 "),
            "git sees a content change only, no mode change"
        );
    }

    #[test]
    fn discard_on_an_unborn_branch_deletes_untracked_files() {
        let fx = Fixture::empty_dir();
        fx.git(&["init", "-q", "-b", "main"]);
        fx.write("first.txt", b"first\n");
        fx.write("staged.txt", b"staged\n");
        fx.git(&["add", "staged.txt"]);

        let outcomes = discard_files(
            fx.path(),
            &Policy::default(),
            &["first.txt".to_owned(), "staged.txt".to_owned()],
            None,
        )
        .unwrap();
        assert!(
            outcomes
                .iter()
                .all(|done| done.kind == DiscardKind::Deleted),
            "{outcomes:?}"
        );
        assert!(!fx.path().join("first.txt").exists());
        assert!(!fx.path().join("staged.txt").exists());
        assert_eq!(fx.git_output(&["ls-files"]), "", "the index is empty again");
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn discard_hunks_refuses_bad_selections() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"edited\n");
        fx.write("untracked.txt", b"junk\n");
        let stamp = stamps(&fx, "alpha.txt")[0];

        let err = discard_hunks(fx.path(), &policy, "alpha.txt", &[], None).unwrap_err();
        assert!(
            matches!(&err, ActionError::NoHunksSelected { op: "discard", path } if path == "alpha.txt"),
            "{err}"
        );
        assert_eq!(
            err.to_string(),
            "discard: no hunks selected for 'alpha.txt'; select a hunk or the whole file"
        );
        for (path, selection) in [("untracked.txt", &[stamp][..]), ("link", &[stamp][..])] {
            let err = discard_hunks(fx.path(), &policy, path, selection, None).unwrap_err();
            assert!(
                matches!(&err, ActionError::HunkMismatch { path: named } if named == path),
                "{path}: {err}"
            );
        }
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"edited\n"
        );
        assert!(fx.path().join("untracked.txt").exists());
        assert_eq!(
            std::fs::read_link(fx.path().join("link")).unwrap().to_str(),
            Some("README.md")
        );
    }

    #[test]
    fn discarding_one_hunk_keeps_the_other() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("hunky.txt", b"l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n");
        fx.commit("Add hunky", "2024-01-05T00:00:00+0000");
        fx.write(
            "hunky.txt",
            b"l1\nCHANGED\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nAPPENDED\n",
        );

        let found = stamps(&fx, "hunky.txt");
        assert_eq!(found.len(), 2);
        discard_hunks(fx.path(), &policy, "hunky.txt", &[found[0]], None).unwrap();
        assert_eq!(
            std::fs::read(fx.path().join("hunky.txt")).unwrap(),
            b"l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nAPPENDED\n",
            "the first hunk's change is gone, the appended line survives"
        );

        let err = discard_hunks(fx.path(), &policy, "hunky.txt", &[found[0]], None).unwrap_err();
        assert!(matches!(err, ActionError::HunkMismatch { .. }), "{err}");
    }

    /// Paths git refuses: inside `.git`, or spelled with `.`, `..` or empty segments.
    pub(super) const INVALID_PATHS: &[&str] = &[
        ".git/config",
        ".git/HEAD",
        ".git/refs/heads/x",
        "./f",
        "./alpha.txt",
        "a//b",
        "/f",
        "a/../f",
    ];

    /// Everything a refused write must leave alone: HEAD, the index, the repository config,
    /// and the worktree files the invalid paths alias.
    #[expect(clippy::unwrap_used, reason = "test helper panics on fixture failure")]
    pub(super) fn untouched_state(fx: &Fixture) -> Vec<String> {
        vec![
            fx.rev_parse("HEAD"),
            fx.git_output(&["ls-files", "--stage"]),
            String::from_utf8_lossy(&std::fs::read(fx.git_dir().join("config")).unwrap())
                .into_owned(),
            String::from_utf8_lossy(&std::fs::read(fx.path().join("alpha.txt")).unwrap())
                .into_owned(),
            String::from_utf8_lossy(&std::fs::read(fx.path().join("f")).unwrap()).into_owned(),
            porcelain(fx),
        ]
    }

    #[test]
    fn commit_and_discard_refuse_invalid_paths_before_writing() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("f", b"untracked f\n");
        fx.write("alpha.txt", b"dirty alpha\n");
        let head = fx.rev_parse("HEAD");
        let before = untouched_state(&fx);

        for path in INVALID_PATHS {
            let err = commit(
                fx.path(),
                &policy,
                CommitRequest {
                    message: "bad path",
                    author: explicit(),
                    expected_head: Some(commit_id(&head)),
                    selections: &[whole(path)],
                },
                None,
            )
            .unwrap_err();
            assert!(
                matches!(&err, ActionError::Git { op: "commit", name, .. } if name == path),
                "commit {path}: {err}"
            );
            let err = discard_files(fx.path(), &policy, &[(*path).to_owned()], None).unwrap_err();
            assert!(
                matches!(&err, ActionError::Git { op: "discard", name, .. } if name == path),
                "discard {path}: {err}"
            );
            assert_eq!(untouched_state(&fx), before, "{path} changed something");
        }

        // A valid path listed first is not discarded when a later one is refused.
        let err = discard_files(
            fx.path(),
            &policy,
            &["alpha.txt".to_owned(), ".git/config".to_owned()],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{err}");
        assert_eq!(untouched_state(&fx), before);
        assert!(fx.git_dir().join("config").exists());
    }

    #[test]
    fn discarding_the_checked_out_branch_ref_file_is_refused() {
        let fx = Fixture::standard();
        let main = fx.rev_parse("main");
        let err = discard_files(
            fx.path(),
            &Policy::default(),
            &[".git/refs/heads/main".to_owned()],
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains(".git/refs/heads/main"), "{err}");
        assert_eq!(fx.rev_parse("refs/heads/main"), main, "the branch survives");
    }

    #[test]
    fn discarding_a_head_path_under_dot_git_writes_no_hook() {
        let fx = Fixture::standard();
        let evil = fx.commit_raw_path(
            ".GIT/hooks/pre-commit",
            b"#!/bin/sh\necho hooked\n",
            "100755",
        );
        fx.git(&["update-ref", "refs/heads/main", &evil]);

        let err = discard_files(
            fx.path(),
            &Policy::default(),
            &[".GIT/hooks/pre-commit".to_owned()],
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, ActionError::Git { op: "discard", .. }),
            "{err}"
        );
        assert!(!fx.git_dir().join("hooks/pre-commit").exists());
    }

    #[test]
    fn discard_never_deletes_a_tracked_file_named_by_another_spelling() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"edited\n");

        for path in ["./alpha.txt", "ALPHA.TXT", "missing.txt"] {
            let err = discard_files(fx.path(), &policy, &[path.to_owned()], None).unwrap_err();
            assert!(
                matches!(&err, ActionError::Git { op: "discard", name, .. } if name == path),
                "{path}: {err}"
            );
            assert_eq!(
                std::fs::read(fx.path().join("alpha.txt")).unwrap(),
                b"edited\n",
                "{path} touched alpha.txt"
            );
        }
    }

    #[test]
    fn a_path_through_a_symlinked_directory_is_refused() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        let outside = Fixture::empty_dir();
        outside.write("secret.txt", b"secret\n");
        fx.symlink(outside.path().to_str().unwrap(), "up");
        let head = fx.rev_parse("HEAD");

        let err =
            discard_files(fx.path(), &policy, &["up/secret.txt".to_owned()], None).unwrap_err();
        assert!(err.to_string().contains("symbolic link"), "{err}");
        let err = commit(
            fx.path(),
            &policy,
            CommitRequest {
                message: "outside",
                author: explicit(),
                expected_head: Some(commit_id(&head)),
                selections: &[whole("up/secret.txt")],
            },
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("symbolic link"), "{err}");

        assert_eq!(
            std::fs::read(outside.path().join("secret.txt")).unwrap(),
            b"secret\n"
        );
        assert_eq!(fx.rev_parse("HEAD"), head);
    }

    /// The message every refusal of a path through a symlinked directory carries.
    pub(super) const CROSSES_LINK: &str = "which would leave the working tree";

    /// Replaces the standard fixture's tracked `docs/` with a link to a fresh directory
    /// holding `guide.md` and `extra.md`, and returns that directory.
    #[expect(clippy::unwrap_used, reason = "test helper panics on fixture failure")]
    pub(super) fn link_docs_outside(fx: &Fixture) -> Fixture {
        let outside = Fixture::empty_dir();
        outside.write("guide.md", b"outside guide\n");
        outside.write("extra.md", b"outside extra\n");
        fx.remove("docs");
        fx.symlink(outside.path().to_str().unwrap(), "docs");
        outside
    }

    #[test]
    fn discard_through_a_symlinked_tracked_directory_leaves_its_target_alone() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        let outside = link_docs_outside(&fx);

        // Restore: docs/guide.md is tracked, so discard would write HEAD's bytes through docs.
        // Delete: docs/extra.md is untracked, so discard would remove it through docs.
        for path in ["docs/guide.md", "docs/extra.md"] {
            let err = discard_files(fx.path(), &policy, &[path.to_owned()], None).unwrap_err();
            assert!(
                matches!(&err, ActionError::Git { op: "discard", name, .. } if name == path),
                "{path}: {err}"
            );
            assert!(err.to_string().contains(CROSSES_LINK), "{path}: {err}");
        }
        let stamp = HunkStamp {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
        };
        let err = discard_hunks(fx.path(), &policy, "docs/guide.md", &[stamp], None).unwrap_err();
        assert!(err.to_string().contains(CROSSES_LINK), "{err}");

        assert_eq!(
            std::fs::read(outside.path().join("guide.md")).unwrap(),
            b"outside guide\n"
        );
        assert_eq!(
            std::fs::read(outside.path().join("extra.md")).unwrap(),
            b"outside extra\n"
        );
    }

    #[test]
    fn discard_refuses_dotdot_and_symlinked_parents() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        let outside = Fixture::empty_dir();
        outside.write("escape.txt", b"outside\n");
        assert_eq!(
            outside.path().parent(),
            fx.path().parent(),
            "both fixtures sit in the same temp dir, so `..` reaches the outside one"
        );
        let name = outside.path().file_name().unwrap().to_str().unwrap();
        fx.symlink(outside.path().to_str().unwrap(), "up");

        for path in [
            format!("../{name}/escape.txt"),
            "up/escape.txt".to_owned(),
            "src/../../escape.txt".to_owned(),
        ] {
            let err =
                discard_files(fx.path(), &policy, std::slice::from_ref(&path), None).unwrap_err();
            assert!(
                matches!(&err, ActionError::Git { op: "discard", name, .. } if *name == path),
                "{path}: {err}"
            );
            assert_eq!(
                std::fs::read(outside.path().join("escape.txt")).unwrap(),
                b"outside\n",
                "{path} reached the outside file"
            );
        }
    }
}

mod stash {
    #![expect(
        clippy::disallowed_methods,
        reason = "tests arrange the fixture's working tree; only the guard mutates the repository"
    )]

    use rc_git_guard::Policy;
    use rc_git_guard::actions::{
        ActionError, Author, Identity, StashRequest, stash_apply, stash_drop, stash_pop, stash_push,
    };
    use rc_git_testkit::Fixture;

    use super::commit_and_discard::file_mode;
    use super::{explicit, porcelain, refs};

    fn stash_list(fx: &Fixture) -> String {
        fx.git_output(&["stash", "list", "--format=%gs"])
    }

    /// The stash commit git calls `stash@{index}`, as the app shows it next to the entry.
    #[expect(
        clippy::unwrap_used,
        reason = "test helper panics on a malformed stash id"
    )]
    fn stash_oid(fx: &Fixture, index: usize) -> rc_git_core::Oid {
        rc_git_core::Oid::from_hex(&fx.rev_parse(&format!("stash@{{{index}}}"))).unwrap()
    }

    #[test]
    fn drop_refuses_an_entry_that_is_not_the_one_shown() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for label in ["one", "two"] {
            fx.write("alpha.txt", format!("{label}\n").as_bytes());
            fx.git(&["stash", "push", "-q", "-m", label]);
        }
        let log_before = std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap();
        let refs_before = refs(&fx);

        let err = stash_drop(fx.path(), &policy, 1, stash_oid(&fx, 0), Some("drop")).unwrap_err();
        assert!(
            matches!(err, ActionError::StaleRepository { op: "stash drop" }),
            "{err}"
        );
        let err = stash_drop(fx.path(), &policy, 2, stash_oid(&fx, 0), Some("drop")).unwrap_err();
        assert!(
            matches!(err, ActionError::StaleRepository { .. }),
            "a position past the list: {err}"
        );

        assert_eq!(stash_list(&fx), "On main: two\nOn main: one");
        assert_eq!(refs(&fx), refs_before, "no backup ref was written");
        assert_eq!(
            std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap(),
            log_before
        );
    }

    #[test]
    fn pop_refuses_after_a_push_shifts_the_entry_it_was_shown() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"shown\n");
        fx.git(&["stash", "push", "-q", "-m", "shown"]);
        let shown = stash_oid(&fx, 0);
        fx.write("alpha.txt", b"pushed from a terminal\n");
        fx.git(&["stash", "push", "-q", "-m", "terminal"]);

        let err = stash_pop(fx.path(), &policy, 0, shown, None).unwrap_err();
        assert!(
            matches!(err, ActionError::StaleRepository { op: "stash pop" }),
            "{err}"
        );
        let err = stash_apply(fx.path(), &policy, 0, shown, None).unwrap_err();
        assert!(
            matches!(err, ActionError::StaleRepository { op: "stash apply" }),
            "{err}"
        );
        assert_eq!(porcelain(&fx), "", "nothing was applied");
        assert_eq!(stash_list(&fx), "On main: terminal\nOn main: shown");

        stash_pop(fx.path(), &policy, 1, shown, None).unwrap();
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"shown\n"
        );
        assert_eq!(stash_list(&fx), "On main: terminal");
    }

    #[test]
    fn a_pop_while_the_stash_log_is_locked_writes_nothing() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for label in ["one", "two"] {
            fx.write("alpha.txt", format!("{label}\n").as_bytes());
            fx.git(&["stash", "push", "-q", "-m", label]);
        }
        let shown = stash_oid(&fx, 1);
        let log_before = std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap();
        let refs_before = refs(&fx);
        let lock = fx.path().join(".git/logs/refs/stash.lock");
        std::fs::write(&lock, b"").unwrap();

        let err = stash_pop(fx.path(), &policy, 1, shown, None).unwrap_err();
        std::fs::remove_file(&lock).unwrap();

        assert!(
            matches!(&err, ActionError::Git { op: "stash pop", name, .. } if name == "refs/stash"),
            "{err}"
        );
        assert_eq!(porcelain(&fx), "", "nothing was applied");
        assert_eq!(
            std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap(),
            log_before,
            "no reflog line was appended or removed"
        );
        assert_eq!(refs(&fx), refs_before, "no backup ref was written");
        assert_eq!(stash_list(&fx), "On main: two\nOn main: one");

        stash_pop(fx.path(), &policy, 1, shown, None).unwrap();
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"one\n"
        );
        assert_eq!(stash_list(&fx), "On main: two");
    }

    #[test]
    fn a_pop_whose_drop_fails_says_the_changes_were_applied() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for label in ["one", "two"] {
            fx.write("alpha.txt", format!("{label}\n").as_bytes());
            fx.git(&["stash", "push", "-q", "-m", label]);
        }
        let shown = stash_oid(&fx, 0);
        let lock = fx.path().join(".git/refs/stash.lock");
        std::fs::write(&lock, b"").unwrap();

        let err = stash_pop(fx.path(), &policy, 0, shown, None).unwrap_err();
        std::fs::remove_file(&lock).unwrap();

        assert!(
            matches!(&err, ActionError::Git { op: "stash pop: drop", name, .. } if name == "stash@{0}"),
            "{err}"
        );
        assert!(
            err.to_string()
                .contains("the changes were applied, but the entry was not dropped"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"two\n"
        );
        assert_eq!(stash_list(&fx), "On main: two\nOn main: one");

        stash_drop(fx.path(), &policy, 0, shown, Some("drop")).unwrap();
        assert_eq!(stash_list(&fx), "On main: one");
    }

    #[test]
    fn a_drop_retried_after_a_failure_keeps_one_backup() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for label in ["one", "two"] {
            fx.write("alpha.txt", format!("{label}\n").as_bytes());
            fx.git(&["stash", "push", "-q", "-m", label]);
        }
        let shown = stash_oid(&fx, 0);
        let top = fx.rev_parse("stash@{0}");
        let lock = fx.path().join(".git/refs/stash.lock");
        std::fs::write(&lock, b"").unwrap();
        stash_drop(fx.path(), &policy, 0, shown, Some("drop")).unwrap_err();
        std::fs::remove_file(&lock).unwrap();

        let deleted = stash_drop(fx.path(), &policy, 0, shown, Some("drop")).unwrap();
        assert_eq!(
            deleted.backup_ref.as_deref(),
            Some("refs/rc-git/backup/stash/0")
        );
        assert_eq!(fx.rev_parse("refs/rc-git/backup/stash/0"), top);
        let listing = refs(&fx);
        assert_eq!(
            listing.matches("refs/rc-git/backup").count(),
            1,
            "{listing}"
        );
        assert_eq!(stash_list(&fx), "On main: one");
    }

    #[test]
    fn an_entry_with_untracked_files_is_refused_and_left_for_git() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"edited\n");
        fx.write("precious.txt", b"precious\n");
        fx.write("notes/draft.md", b"draft\n");
        fx.git(&["stash", "push", "-u", "-q", "-m", "with untracked"]);
        let shown = stash_oid(&fx, 0);
        let refs_before = refs(&fx);

        let err = stash_pop(fx.path(), &policy, 0, shown, None).unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "stash pop", name, .. } if name == "stash@{0}"),
            "{err}"
        );
        let message = err.to_string();
        for expected in ["notes/draft.md", "precious.txt", "git stash pop stash@{0}"] {
            assert!(message.contains(expected), "{message}");
        }
        let err = stash_apply(fx.path(), &policy, 0, shown, None).unwrap_err();
        assert!(
            matches!(
                &err,
                ActionError::Git {
                    op: "stash apply",
                    ..
                }
            ),
            "{err}"
        );
        assert!(
            err.to_string().contains("git stash apply stash@{0}"),
            "{err}"
        );

        assert_eq!(porcelain(&fx), "", "nothing was applied");
        assert_eq!(refs(&fx), refs_before, "no backup ref was written");
        assert_eq!(stash_list(&fx), "On main: with untracked");

        fx.git(&["stash", "pop", "-q"]);
        assert_eq!(
            std::fs::read(fx.path().join("precious.txt")).unwrap(),
            b"precious\n",
            "the entry git was handed still restores the untracked file"
        );
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"edited\n"
        );
    }

    #[test]
    fn an_untracked_refusal_names_a_few_files_and_says_there_are_more() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for n in 0..8 {
            fx.write(&format!("new/{n}.txt"), b"x\n");
        }
        fx.git(&["stash", "push", "-u", "-q"]);

        let err = stash_apply(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("(new/0.txt, new/1.txt, new/2.txt, new/3.txt, new/4.txt and more)"),
            "{message}"
        );
    }

    #[test]
    fn a_stash_push_also_unstages_the_stashed_paths() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"staged\n");
        fx.write("fresh.txt", b"new and staged\n");
        fx.git(&["add", "alpha.txt", "fresh.txt"]);

        stash_push(
            fx.path(),
            &Policy::default(),
            StashRequest {
                paths: &["alpha.txt".to_owned(), "fresh.txt".to_owned()],
                message: None,
                author: explicit(),
            },
            None,
        )
        .unwrap();
        assert_eq!(
            porcelain(&fx),
            "",
            "nothing is left staged for a plain git commit"
        );

        fx.git(&["stash", "pop", "-q"]);
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"staged\n"
        );
        assert_eq!(
            std::fs::read(fx.path().join("fresh.txt")).unwrap(),
            b"new and staged\n"
        );
    }

    #[test]
    fn a_pushed_stash_is_one_real_git_can_list_and_pop() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"stashed content\n");
        fx.write("brand-new.txt", b"untracked goes in too\n");

        let saved = stash_push(
            fx.path(),
            &policy,
            StashRequest {
                paths: &["alpha.txt".to_owned(), "brand-new.txt".to_owned()],
                message: Some("half-done work"),
                author: explicit(),
            },
            None,
        )
        .unwrap();
        assert_eq!(saved.message, "On main: half-done work");
        assert_eq!(saved.oid, fx.rev_parse("stash@{0}"));
        assert_eq!(stash_list(&fx), "On main: half-done work");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n",
            "the stashed file is back at HEAD content"
        );
        assert!(!fx.path().join("brand-new.txt").exists());
        assert_eq!(porcelain(&fx), "");

        // The compat proof: the real git pops what rc-git pushed.
        fx.git(&["stash", "pop", "-q"]);
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"stashed content\n"
        );
        assert_eq!(
            std::fs::read(fx.path().join("brand-new.txt")).unwrap(),
            b"untracked goes in too\n"
        );
    }

    #[test]
    fn a_multi_line_message_logs_one_reflog_line_and_keeps_the_body_in_the_commit() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"stashed content\n");

        let saved = stash_push(
            fx.path(),
            &Policy::default(),
            StashRequest {
                paths: &["alpha.txt".to_owned()],
                message: Some("subject\n\nbody"),
                author: explicit(),
            },
            None,
        )
        .unwrap();
        assert_eq!(saved.message, "On main: subject body");
        assert_eq!(stash_list(&fx), "On main: subject body");
        let core = rc_git_core::Repository::open(fx.path()).unwrap();
        let listed = core.stash_list().unwrap();
        let messages: Vec<&str> = listed.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, ["On main: subject body"]);
        let reflog = std::fs::read_to_string(fx.path().join(".git/logs/refs/stash")).unwrap();
        assert_eq!(reflog.lines().count(), 1, "{reflog}");
        assert_eq!(
            fx.git_output(&["log", "-1", "--format=%B", "refs/stash"]),
            "On main: subject\n\nbody",
            "the stash commit keeps the full message"
        );
    }

    #[test]
    fn a_mode_only_change_stashes_and_pops_the_exec_bit() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.set_mode("alpha.txt", 0o755);

        stash_push(
            fx.path(),
            &policy,
            StashRequest {
                paths: &["alpha.txt".to_owned()],
                message: None,
                author: explicit(),
            },
            None,
        )
        .unwrap();
        assert_eq!(
            file_mode(&fx, "alpha.txt"),
            0o644,
            "push restores HEAD's mode"
        );
        assert_eq!(porcelain(&fx), "");

        stash_pop(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap();
        assert_eq!(
            file_mode(&fx, "alpha.txt"),
            0o755,
            "pop brings the exec bit back"
        );
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n"
        );
        assert_eq!(porcelain(&fx), " M alpha.txt");
        assert_eq!(stash_list(&fx), "");
    }

    #[test]
    fn apply_turns_a_file_into_a_directory() {
        let fx = Fixture::standard();
        fx.git(&["rm", "-q", "alpha.txt"]);
        fx.write("alpha.txt/y", b"y\n");
        fx.git(&["add", "alpha.txt/y"]);
        fx.git(&["stash", "push", "-q"]);
        assert_eq!(porcelain(&fx), "");

        stash_apply(fx.path(), &Policy::default(), 0, stash_oid(&fx, 0), None).unwrap();
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt/y")).unwrap(),
            b"y\n"
        );
        assert_eq!(
            porcelain(&fx),
            " D alpha.txt",
            "git hides alpha.txt/ behind the index entry"
        );
    }

    #[test]
    fn apply_turns_a_directory_into_a_file() {
        let fx = Fixture::standard();
        fx.git(&["rm", "-rq", "docs"]);
        fx.write("docs", b"now a file\n");
        fx.git(&["add", "docs"]);
        fx.git(&["stash", "push", "-q"]);
        assert_eq!(porcelain(&fx), "");

        stash_apply(fx.path(), &Policy::default(), 0, stash_oid(&fx, 0), None).unwrap();
        assert_eq!(
            std::fs::read(fx.path().join("docs")).unwrap(),
            b"now a file\n"
        );
        assert_eq!(porcelain(&fx), " D docs/guide.md\n?? docs");
    }

    #[test]
    fn apply_refuses_a_directory_to_file_change_over_an_untracked_file() {
        let fx = Fixture::standard();
        fx.git(&["rm", "-rq", "docs"]);
        fx.write("docs", b"now a file\n");
        fx.git(&["add", "docs"]);
        fx.git(&["stash", "push", "-q"]);
        fx.write("docs/notes.md", b"mine\n");

        let err =
            stash_apply(fx.path(), &Policy::default(), 0, stash_oid(&fx, 0), None).unwrap_err();
        assert!(
            matches!(&err, ActionError::DirtyPath { path, .. } if path == "docs/notes.md"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(fx.path().join("docs/guide.md")).unwrap(),
            b"guide\n",
            "nothing was written"
        );
        assert_eq!(porcelain(&fx), "?? docs/notes.md");
    }

    #[test]
    fn a_nested_path_stashes_and_pops_with_real_git() {
        let fx = Fixture::standard();
        fx.write("src/lib.rs", b"pub fn two() -> u32 {\n    2\n}\n");

        stash_push(
            fx.path(),
            &Policy::default(),
            StashRequest {
                paths: &["src/lib.rs".to_owned()],
                message: None,
                author: explicit(),
            },
            None,
        )
        .unwrap();
        assert_eq!(
            fx.git_output(&["show", "stash@{0}:src/lib.rs"]),
            "pub fn two() -> u32 {\n    2\n}"
        );
        assert_eq!(
            fx.rev_parse("stash@{0}:docs"),
            fx.rev_parse("HEAD:docs"),
            "an untouched sibling folder keeps its tree id"
        );
        assert_eq!(
            fx.rev_parse("stash@{0}:src/main.rs"),
            fx.rev_parse("HEAD:src/main.rs")
        );
        assert_eq!(porcelain(&fx), "");

        fx.git(&["stash", "pop", "-q"]);
        assert_eq!(
            std::fs::read(fx.path().join("src/lib.rs")).unwrap(),
            b"pub fn two() -> u32 {\n    2\n}\n"
        );
        assert_eq!(porcelain(&fx), " M src/lib.rs");
    }

    #[test]
    fn a_stash_records_the_repo_identity_over_the_fallback() {
        let fx = Fixture::standard();
        fx.git(&["config", "user.name", "Repo Owner"]);
        fx.git(&["config", "user.email", "repo@example.com"]);
        fx.write("alpha.txt", b"stash me\n");

        let author = Author {
            explicit: None,
            fallback: Some(Identity {
                name: "Saved Fallback".to_owned(),
                email: "saved@example.com".to_owned(),
            }),
        };
        stash_push(
            fx.path(),
            &Policy::default(),
            StashRequest {
                paths: &["alpha.txt".to_owned()],
                message: None,
                author,
            },
            None,
        )
        .unwrap();
        assert_eq!(
            fx.git_output(&["log", "-1", "--format=%ae|%ce", "stash@{0}"]),
            "repo@example.com|repo@example.com"
        );
    }

    #[test]
    fn push_refuses_a_head_directory_before_writing_anything() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"stash me\n");
        fx.remove("src");

        let err = stash_push(
            fx.path(),
            &policy,
            StashRequest {
                paths: &["alpha.txt".to_owned(), "src".to_owned()],
                message: None,
                author: explicit(),
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{err}");
        assert!(
            err.to_string().contains("'src'") && err.to_string().contains("is a directory"),
            "{err}"
        );
        assert_eq!(stash_list(&fx), "", "no stash entry was written");
        assert!(!refs(&fx).contains("refs/stash"));
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"stash me\n",
            "the edit stays on disk"
        );
    }

    #[test]
    fn our_pop_restores_a_stash_the_real_git_pushed() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"from real git\n");
        fx.git(&["stash", "push", "-q", "-m", "real one"]);

        let deleted = stash_pop(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap();
        assert_eq!(deleted.full_name, "stash@{0}");
        assert_eq!(
            deleted.backup_ref.as_deref(),
            Some("refs/rc-git/backup/stash/0")
        );
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"from real git\n"
        );
        assert_eq!(stash_list(&fx), "", "the entry is gone");
        assert_eq!(
            porcelain(&fx),
            " M alpha.txt".to_owned(),
            "the restored change is an ordinary unstaged edit"
        );
    }

    #[test]
    fn dropping_the_middle_of_three_keeps_the_others_in_order() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for label in ["one", "two", "three"] {
            fx.write("alpha.txt", format!("{label}\n").as_bytes());
            fx.git(&["stash", "push", "-q", "-m", label]);
        }
        assert_eq!(
            stash_list(&fx),
            "On main: three\nOn main: two\nOn main: one"
        );
        let two = fx.rev_parse("stash@{1}");

        let err = stash_drop(fx.path(), &policy, 1, stash_oid(&fx, 1), Some("wrong")).unwrap_err();
        assert!(matches!(err, ActionError::NotConfirmed { .. }), "{err}");
        assert_eq!(
            stash_list(&fx),
            "On main: three\nOn main: two\nOn main: one"
        );

        let deleted = stash_drop(fx.path(), &policy, 1, stash_oid(&fx, 1), Some("drop")).unwrap();
        assert_eq!(deleted.full_name, "stash@{1}");
        assert_eq!(
            deleted.backup_ref.as_deref(),
            Some("refs/rc-git/backup/stash/1")
        );
        assert_eq!(stash_list(&fx), "On main: three\nOn main: one");
        assert_eq!(
            fx.rev_parse("refs/rc-git/backup/stash/1"),
            two,
            "the backup keeps the dropped stash commit"
        );
        // The rewritten reflog is still one git accepts end to end.
        fx.git(&["stash", "drop", "-q", "stash@{0}"]);
        assert_eq!(stash_list(&fx), "On main: one");
    }

    #[test]
    fn dropping_the_newest_updates_the_ref_and_dropping_the_last_deletes_it() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for label in ["one", "two"] {
            fx.write("alpha.txt", format!("{label}\n").as_bytes());
            fx.git(&["stash", "push", "-q", "-m", label]);
        }
        let one = fx.rev_parse("stash@{1}");

        stash_drop(fx.path(), &policy, 0, stash_oid(&fx, 0), Some("drop")).unwrap();
        assert_eq!(stash_list(&fx), "On main: one");
        assert_eq!(fx.rev_parse("refs/stash"), one);

        stash_drop(fx.path(), &policy, 0, stash_oid(&fx, 0), Some("drop")).unwrap();
        assert_eq!(stash_list(&fx), "");
        assert!(!refs(&fx).contains("refs/stash"));
    }

    #[test]
    fn dropping_the_oldest_of_three_rechains_the_new_oldest_to_the_null_oid() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for label in ["one", "two", "three"] {
            fx.write("alpha.txt", format!("{label}\n").as_bytes());
            fx.git(&["stash", "push", "-q", "-m", label]);
        }
        let top = fx.rev_parse("refs/stash");
        let one = fx.rev_parse("stash@{2}");

        let deleted = stash_drop(fx.path(), &policy, 2, stash_oid(&fx, 2), Some("drop")).unwrap();
        assert_eq!(deleted.full_name, "stash@{2}");
        assert_eq!(fx.rev_parse("refs/rc-git/backup/stash/2"), one);
        assert_eq!(
            fx.rev_parse("refs/stash"),
            top,
            "the newest entry stays on top"
        );
        assert_eq!(stash_list(&fx), "On main: three\nOn main: two");
        assert_eq!(
            fx.git_output(&["reflog", "show", "--format=%gs", "refs/stash"]),
            "On main: three\nOn main: two"
        );
        let log = std::fs::read_to_string(fx.path().join(".git/logs/refs/stash")).unwrap();
        let oldest = log.lines().next().unwrap();
        assert!(
            oldest.starts_with(&format!("{} {}", "0".repeat(40), fx.rev_parse("stash@{1}"))),
            "{log}"
        );
    }

    #[test]
    fn a_stash_index_past_the_end_is_refused_and_writes_nothing() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"stashed\n");
        fx.git(&["stash", "push", "-q", "-m", "only"]);
        let shown = stash_oid(&fx, 0);
        let refs_before = refs(&fx);

        let err = stash_apply(fx.path(), &policy, 1, shown, None).unwrap_err();
        assert!(
            matches!(err, ActionError::StaleRepository { op: "stash apply" }),
            "{err}"
        );
        let err = stash_pop(fx.path(), &policy, 1, shown, None).unwrap_err();
        assert!(
            matches!(err, ActionError::StaleRepository { op: "stash pop" }),
            "{err}"
        );
        let err = stash_drop(fx.path(), &policy, 1, shown, Some("drop")).unwrap_err();
        assert!(
            matches!(err, ActionError::StaleRepository { op: "stash drop" }),
            "{err}"
        );

        assert_eq!(stash_list(&fx), "On main: only");
        assert_eq!(refs(&fx), refs_before, "no backup ref was written");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n",
            "nothing was applied"
        );
    }

    #[test]
    fn stash_actions_on_a_repository_without_a_stash_are_refused() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        let any = rc_git_core::Oid::from_hex(&fx.rev_parse("HEAD")).unwrap();
        let refs_before = refs(&fx);

        let err = stash_apply(fx.path(), &policy, 0, any, None).unwrap_err();
        assert!(
            matches!(err, ActionError::StaleRepository { op: "stash apply" }),
            "{err}"
        );
        let err = stash_pop(fx.path(), &policy, 0, any, None).unwrap_err();
        assert!(
            matches!(err, ActionError::StaleRepository { op: "stash pop" }),
            "{err}"
        );
        let err = stash_drop(fx.path(), &policy, 0, any, Some("drop")).unwrap_err();
        assert!(
            matches!(err, ActionError::StaleRepository { op: "stash drop" }),
            "{err}"
        );

        assert_eq!(refs(&fx), refs_before, "no stash or backup ref was written");
        assert!(!fx.path().join(".git/logs/refs/stash").exists());
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn popping_a_stashed_rename_restores_the_new_path_and_removes_the_old() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.git(&["mv", "alpha.txt", "renamed.txt"]);
        fx.git(&["stash", "push", "-q", "-m", "moved"]);
        assert_eq!(porcelain(&fx), "");

        stash_pop(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap();
        assert!(!fx.path().join("alpha.txt").exists());
        assert_eq!(
            std::fs::read(fx.path().join("renamed.txt")).unwrap(),
            b"alpha\n"
        );
        assert_eq!(stash_list(&fx), "");
    }

    #[test]
    fn popping_a_stashed_symlink_and_executable_restores_their_modes() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.symlink("alpha.txt", "stashed-link");
        fx.write("run.sh", b"#!/bin/sh\n");
        fx.set_mode("run.sh", 0o755);
        fx.git(&["add", "stashed-link", "run.sh"]);
        fx.git(&["stash", "push", "-q", "-m", "modes"]);
        assert!(std::fs::symlink_metadata(fx.path().join("stashed-link")).is_err());

        stash_pop(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap();
        let link = fx.path().join("stashed-link");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            std::path::Path::new("alpha.txt")
        );
        assert_eq!(file_mode(&fx, "run.sh"), 0o755);
        assert_eq!(
            std::fs::read(fx.path().join("run.sh")).unwrap(),
            b"#!/bin/sh\n"
        );
        assert_eq!(stash_list(&fx), "");
    }

    #[test]
    fn a_drop_while_the_stash_log_is_locked_changes_nothing() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for label in ["one", "two"] {
            fx.write("alpha.txt", format!("{label}\n").as_bytes());
            fx.git(&["stash", "push", "-q", "-m", label]);
        }
        let top = fx.rev_parse("refs/stash");
        let log_before = std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap();
        let refs_before = refs(&fx);
        fx.write(".git/logs/refs/stash.lock", b"");

        let err = stash_drop(fx.path(), &policy, 0, stash_oid(&fx, 0), Some("drop")).unwrap_err();
        assert!(err.to_string().contains("stash drop"), "{err}");
        assert_eq!(fx.rev_parse("refs/stash"), top, "the ref did not move");
        assert_eq!(
            std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap(),
            log_before,
            "no reflog line was appended or removed"
        );
        assert_eq!(refs(&fx), refs_before, "no backup ref was written");
        assert_eq!(stash_list(&fx), "On main: two\nOn main: one");
    }

    #[test]
    fn dropping_past_a_junk_reflog_line_drops_the_entry_git_lists() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for label in ["kept", "second"] {
            fx.write("alpha.txt", format!("{label}\n").as_bytes());
            fx.git(&["stash", "push", "-q", "-m", label]);
        }
        let older = fx.rev_parse("stash@{1}");
        let mut log = std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap();
        log.extend_from_slice(b"junk\n");
        fx.write(".git/logs/refs/stash", &log);

        stash_drop(fx.path(), &policy, 1, stash_oid(&fx, 1), Some("drop")).unwrap();
        assert_eq!(stash_list(&fx), "On main: second");
        assert_eq!(
            fx.rev_parse("refs/rc-git/backup/stash/1"),
            older,
            "the backup keeps the stash git called stash@{{1}}"
        );
    }

    #[test]
    fn an_unterminated_reflog_tail_refuses_apply_pop_and_drop() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        for label in ["kept", "second"] {
            fx.write("alpha.txt", format!("{label}\n").as_bytes());
            fx.git(&["stash", "push", "-q", "-m", label]);
        }
        let mut log = std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap();
        assert_eq!(log.pop(), Some(b'\n'));
        fx.write(".git/logs/refs/stash", &log);
        let listed =
            rc_git_core::Oid::from_hex(&fx.git_output(&["stash", "list", "--format=%H"])).unwrap();
        assert_ne!(listed.to_hex(), fx.rev_parse("refs/stash"));
        let top = fx.rev_parse("refs/stash");
        let refs_before = refs(&fx);

        let attempts = [
            (
                "stash apply",
                stash_apply(fx.path(), &policy, 0, listed, None),
            ),
            (
                "stash pop",
                stash_pop(fx.path(), &policy, 0, listed, None).map(|_| ()),
            ),
            (
                "stash drop",
                stash_drop(fx.path(), &policy, 0, listed, Some("drop")).map(|_| ()),
            ),
        ];
        for (op, result) in attempts {
            let err = result.unwrap_err();
            assert!(
                matches!(&err, ActionError::Git { op: got, .. } if *got == op),
                "{err}"
            );
            assert!(
                err.to_string().contains("refs/stash is not the entry"),
                "{err}"
            );
        }
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n",
            "nothing was applied"
        );
        assert_eq!(fx.rev_parse("refs/stash"), top, "the ref did not move");
        assert_eq!(
            std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap(),
            log,
            "the reflog was not rewritten"
        );
        assert_eq!(refs(&fx), refs_before, "no backup ref was written");
    }

    #[test]
    fn apply_refuses_a_stashed_gitlink_before_writing_anything() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"stashed edit\n");
        fx.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,1111111111111111111111111111111111111111,sub",
        ]);
        fx.git(&["stash", "push", "-q", "-m", "with gitlink"]);
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n"
        );

        let err = stash_apply(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{err}");
        assert!(
            err.to_string().contains("'sub'") && err.to_string().contains("submodule"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n",
            "the file sorted before the gitlink was not written"
        );
        assert_eq!(
            stash_list(&fx),
            "On main: with gitlink",
            "the stash survives"
        );
    }

    #[test]
    fn apply_refuses_a_resized_path_from_its_size_alone() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"stash me\n");
        fx.git(&["stash", "push", "-q", "-m", "kept"]);
        fx.write("alpha.txt", b"a longer conflicting edit\n");
        let path = fx.path().join("alpha.txt");
        // Unreadable, so any content read would fail as an error instead of a dirty path.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = stash_apply(fx.path(), &policy, 0, stash_oid(&fx, 0), None);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = result.unwrap_err();
        assert!(
            matches!(&err, ActionError::DirtyPath { op: "stash apply", path } if path == "alpha.txt"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"a longer conflicting edit\n"
        );
        assert_eq!(stash_list(&fx), "On main: kept", "the stash survives");
    }

    #[test]
    fn apply_refuses_a_dirty_path_and_touches_nothing() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"stash me\n");
        fx.git(&["stash", "push", "-q", "-m", "kept"]);
        fx.write("alpha.txt", b"conflicting edit\n");

        let err = stash_apply(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap_err();
        assert!(
            matches!(&err, ActionError::DirtyPath { op: "stash apply", path } if path == "alpha.txt"),
            "{err}"
        );
        assert!(
            err.to_string().starts_with("stash apply: 'alpha.txt'"),
            "{err}"
        );
        let err = stash_pop(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap_err();
        assert!(
            matches!(
                &err,
                ActionError::DirtyPath {
                    op: "stash pop",
                    ..
                }
            ),
            "{err}"
        );
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"conflicting edit\n",
            "nothing was written"
        );
        assert_eq!(stash_list(&fx), "On main: kept", "the stash survives");

        // Clean state again: apply works and keeps the entry.
        fx.git(&["checkout", "-q", "--", "alpha.txt"]);
        stash_apply(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap();
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"stash me\n"
        );
        assert_eq!(stash_list(&fx), "On main: kept");
    }

    #[test]
    fn stashing_a_deleted_file_brings_it_back_and_pop_deletes_it_again() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        std::fs::remove_file(fx.path().join("Zeta.txt")).unwrap();

        stash_push(
            fx.path(),
            &policy,
            StashRequest {
                paths: &["Zeta.txt".to_owned()],
                message: None,
                author: explicit(),
            },
            None,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(fx.path().join("Zeta.txt")).unwrap(),
            b"zeta\n",
            "the deletion is stashed, so the file is back"
        );
        assert!(stash_list(&fx).starts_with("WIP on main:"));

        stash_pop(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap();
        assert!(!fx.path().join("Zeta.txt").exists());
    }

    #[test]
    fn stash_push_on_a_detached_head_names_stash_push() {
        let fx = Fixture::standard();
        fx.git(&["checkout", "-q", "--detach", "HEAD"]);
        fx.write("alpha.txt", b"changed\n");
        let err = stash_push(
            fx.path(),
            &Policy::default(),
            StashRequest {
                paths: &["alpha.txt".to_owned()],
                message: None,
                author: explicit(),
            },
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, ActionError::DetachedHead { op: "stash push" }),
            "{err}"
        );
        assert_eq!(stash_list(&fx), "");
    }

    #[expect(clippy::unwrap_used, reason = "test helper panics on a failed push")]
    fn refused_push(fx: &Fixture, paths: &[&str], author: Author) -> ActionError {
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        stash_push(
            fx.path(),
            &Policy::default(),
            StashRequest {
                paths: &paths,
                message: None,
                author,
            },
            None,
        )
        .unwrap_err()
    }

    fn assert_refused(err: &ActionError, name: &str, message: &str) {
        assert!(
            matches!(err, ActionError::Git { op: "stash push", name: n, .. } if n == name),
            "{err}"
        );
        assert!(err.to_string().contains(message), "{err}");
    }

    #[test]
    fn stash_push_refuses_empty_unchanged_unborn_and_identityless() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"changed\n");

        let err = refused_push(&fx, &[], explicit());
        assert_refused(&err, "refs/stash", "nothing selected to stash");
        let err = refused_push(&fx, &["README.md"], explicit());
        assert_refused(
            &err,
            "refs/stash",
            "no changes to stash at the selected paths",
        );
        let err = refused_push(&fx, &["alpha.txt"], Author::default());
        assert!(
            matches!(err, ActionError::MissingIdentity { op: "stash push" }),
            "{err}"
        );
        assert!(
            err.to_string()
                .starts_with("stash push: no author identity"),
            "{err}"
        );
        assert_eq!(stash_list(&fx), "");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"changed\n",
            "the edit survives every refused push"
        );

        let unborn = Fixture::empty_dir();
        unborn.git(&["init", "-q", "-b", "main"]);
        unborn.write("first.txt", b"first\n");
        let err = refused_push(&unborn, &["first.txt"], explicit());
        assert_refused(&err, "refs/stash", "cannot stash before the first commit");
        assert_eq!(
            std::fs::read(unborn.path().join("first.txt")).unwrap(),
            b"first\n"
        );
    }

    #[test]
    fn stashing_the_deletion_of_every_file_is_refused() {
        let fx = Fixture::empty_dir();
        fx.git(&["init", "-q", "-b", "main"]);
        fx.write("only.txt", b"only\n");
        fx.commit("Only file", "2024-01-05T00:00:00+0000");
        fx.remove("only.txt");

        let err = refused_push(&fx, &["only.txt"], explicit());
        assert_refused(
            &err,
            "refs/stash",
            "stashing every file would empty the tree",
        );
        assert_eq!(stash_list(&fx), "");
        assert!(!fx.path().join("only.txt").exists(), "the deletion stays");
    }

    #[test]
    fn stash_push_refuses_invalid_paths_before_writing() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("f", b"untracked f\n");
        fx.write("alpha.txt", b"dirty alpha\n");
        let before = super::commit_and_discard::untouched_state(&fx);

        for path in super::commit_and_discard::INVALID_PATHS {
            let err = stash_push(
                fx.path(),
                &policy,
                StashRequest {
                    paths: &[(*path).to_owned()],
                    message: None,
                    author: explicit(),
                },
                None,
            )
            .unwrap_err();
            assert!(
                matches!(&err, ActionError::Git { op: "stash push", name, .. } if name == path),
                "{path}: {err}"
            );
            assert_eq!(
                super::commit_and_discard::untouched_state(&fx),
                before,
                "{path} changed something"
            );
        }
        assert_eq!(stash_list(&fx), "");
        assert!(!refs(&fx).contains("refs/stash"));
    }

    #[test]
    fn stash_never_writes_through_a_symlinked_directory() {
        use super::commit_and_discard::{CROSSES_LINK, link_docs_outside};
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("docs/guide.md", b"stashed guide\n");
        stash_push(
            fx.path(),
            &policy,
            StashRequest {
                paths: &["docs/guide.md".to_owned()],
                message: None,
                author: explicit(),
            },
            None,
        )
        .unwrap();
        let stashed = stash_list(&fx);
        let outside = link_docs_outside(&fx);

        let err = stash_push(
            fx.path(),
            &policy,
            StashRequest {
                paths: &["docs/guide.md".to_owned()],
                message: None,
                author: explicit(),
            },
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains(CROSSES_LINK), "push: {err}");
        let err = stash_pop(fx.path(), &policy, 0, stash_oid(&fx, 0), None).unwrap_err();
        assert!(err.to_string().contains(CROSSES_LINK), "pop: {err}");

        assert_eq!(
            std::fs::read(outside.path().join("guide.md")).unwrap(),
            b"outside guide\n"
        );
        assert_eq!(stash_list(&fx), stashed, "the stash survives");
    }
}

mod checkout {
    #![expect(
        clippy::disallowed_methods,
        reason = "tests arrange the fixture's git dir; only the guard mutates the repository"
    )]

    use rc_git_guard::Policy;
    use rc_git_guard::actions::{ActionError, checkout_branch, create_branch};
    use rc_git_testkit::Fixture;

    use super::commit_and_discard::CROSSES_LINK;
    use super::porcelain;

    fn head_branch(fx: &Fixture) -> String {
        fx.git_output(&["rev-parse", "--abbrev-ref", "HEAD"])
    }

    #[test]
    fn a_head_on_a_missing_commit_is_reported_as_a_read_error() {
        let fx = Fixture::standard();
        fx.write(".git/HEAD", format!("{}\n", "1".repeat(40)).as_bytes());

        let err = checkout_branch(fx.path(), &Policy::default(), "feature").unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{err}");
        assert!(err.to_string().contains("HEAD"), "{err}");
        assert!(
            fx.path().join("alpha.txt").exists(),
            "nothing in the working tree moved"
        );
    }

    #[test]
    fn switching_branches_rewrites_the_tree_the_index_and_head() {
        let fx = Fixture::standard();
        let feature_tip = fx.rev_parse("feature");

        let outcome = checkout_branch(fx.path(), &Policy::default(), "feature").unwrap();
        assert_eq!(outcome.branch, "feature");
        assert_eq!(outcome.new_head.to_string(), feature_tip);
        assert!(!outcome.already_on_branch);
        assert!(outcome.files_changed > 0);

        assert_eq!(head_branch(&fx), "feature");
        assert_eq!(fx.rev_parse("HEAD"), feature_tip);
        assert_eq!(
            std::fs::read(fx.path().join("feature.txt")).unwrap(),
            b"feature\n",
            "a file only the target branch has must appear"
        );
        assert!(
            !fx.path().join("alpha.txt").exists(),
            "a file only the old branch has must be gone"
        );
        assert_eq!(porcelain(&fx), "", "the index must agree with the new tree");
        assert_eq!(
            fx.git_output(&["reflog", "show", "--format=%gs", "-1", "HEAD"]),
            "checkout: moving from main to feature",
            "HEAD's reflog carries the switch, so `git checkout -` works afterwards"
        );
    }

    #[test]
    fn an_unwritable_head_reflog_refuses_before_writing() {
        let fx = Fixture::standard();
        fx.set_mode(".git/logs/HEAD", 0o444);

        let err = checkout_branch(fx.path(), &Policy::default(), "feature").unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{err}");
        assert!(err.to_string().contains("logs/HEAD"), "{err}");
        assert_eq!(head_branch(&fx), "main", "HEAD must not move");
        assert!(
            fx.path().join("alpha.txt").exists(),
            "the working tree must not change"
        );
        assert!(
            !fx.path().join("feature.txt").exists(),
            "the working tree must not change"
        );
    }

    #[test]
    fn switching_thousands_of_paths_leaves_an_index_that_matches_the_target_tree() {
        let fx = Fixture::empty_dir();
        fx.git(&["init", "-q", "-b", "main"]);
        for i in 0..3000 {
            fx.write(
                &format!("d{:02}/f{i:05}.txt", i % 50),
                format!("base {i}\n").as_bytes(),
            );
        }
        fx.commit("base", "2024-01-01T00:00:00+0000");
        fx.git(&["switch", "-q", "-c", "other"]);
        for i in (0..3000).step_by(3) {
            fx.write(
                &format!("d{:02}/f{i:05}.txt", i % 50),
                format!("other {i}\n").as_bytes(),
            );
        }
        for i in (1..750).step_by(3) {
            fx.remove(&format!("d{:02}/f{i:05}.txt", i % 50));
        }
        for i in 3000..3250 {
            fx.write(&format!("new/f{i:05}.txt"), format!("new {i}\n").as_bytes());
        }
        fx.commit("other", "2024-01-02T00:00:00+0000");
        fx.git(&["switch", "-q", "main"]);

        let outcome = checkout_branch(fx.path(), &Policy::default(), "other").unwrap();

        assert_eq!(outcome.files_changed, 1000 + 250 + 250);
        assert_eq!(
            fx.git_output(&["ls-files", "--stage"]).lines().count(),
            3000 - 250 + 250,
            "every entry appears once: no stale or duplicate entries survive"
        );
        assert_eq!(
            fx.git_output(&["write-tree"]),
            fx.rev_parse("other^{tree}"),
            "the index content must be exactly the target tree"
        );
        assert_eq!(
            porcelain(&fx),
            "",
            "the index stats must agree with the disk"
        );
    }

    #[test]
    fn ref_edits_and_the_head_switch_line_share_one_reflog_identity() {
        let fx = Fixture::standard();
        create_branch(fx.path(), &Policy::default(), "topic", "main").unwrap();
        checkout_branch(fx.path(), &Policy::default(), "topic").unwrap();

        let identity = |reference: &str| {
            fx.git_output(&["reflog", "show", "--format=%gn <%ge>", "-1", reference])
        };
        assert_eq!(identity("topic"), "rc-git <rc-git@localhost>");
        assert_eq!(
            identity("HEAD"),
            identity("topic"),
            "the hand-written checkout line must carry the same committer as apply_edit"
        );
    }

    #[test]
    fn switching_across_a_copy_updates_the_copy_source_under_diff_renames_copies() {
        let fx = Fixture::empty_dir();
        fx.git(&["init", "-q", "-b", "main"]);
        fx.write("b.txt", b"1\n2\n3\n4\n5\n6\n");
        fx.commit("add", "2024-01-01T00:00:00+0000");
        fx.git(&["branch", "base"]);
        fx.write("c.txt", b"1\n2\n3\n4\n5\n6\n");
        fx.write("b.txt", b"1\n2\n3\n4\n5\n6\n7\n");
        fx.commit("copy", "2024-01-02T00:00:00+0000");
        fx.git(&["config", "diff.renames", "copies"]);
        fx.git(&["switch", "-q", "base"]);

        let outcome = checkout_branch(fx.path(), &Policy::default(), "main").unwrap();

        assert_eq!(outcome.files_changed, 2);
        assert_eq!(head_branch(&fx), "main");
        assert_eq!(
            std::fs::read(fx.path().join("b.txt")).unwrap(),
            b"1\n2\n3\n4\n5\n6\n7\n",
            "the copy source's own change must be written"
        );
        assert_eq!(porcelain(&fx), "", "the index must agree with the new tree");
    }

    #[test]
    fn switching_back_restores_the_first_branch() {
        let fx = Fixture::standard();
        let main_tip = fx.rev_parse("main");
        checkout_branch(fx.path(), &Policy::default(), "feature").unwrap();
        checkout_branch(fx.path(), &Policy::default(), "main").unwrap();

        assert_eq!(head_branch(&fx), "main");
        assert_eq!(fx.rev_parse("HEAD"), main_tip);
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n"
        );
        assert!(!fx.path().join("feature.txt").exists());
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn checking_out_the_current_branch_writes_nothing() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"local work\n");

        let outcome = checkout_branch(fx.path(), &Policy::default(), "main").unwrap();
        assert!(outcome.already_on_branch);
        assert_eq!(outcome.files_changed, 0);
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"local work\n",
            "an uncommitted edit on the current branch is untouched"
        );
    }

    #[test]
    fn a_dirty_path_the_switch_touches_refuses_the_whole_switch() {
        let fx = Fixture::standard();
        // `feature` has no alpha.txt, so the switch must delete it — and it is dirty.
        fx.write("alpha.txt", b"work in progress\n");

        let err = checkout_branch(fx.path(), &Policy::default(), "feature").unwrap_err();
        assert!(
            matches!(&err, ActionError::DirtyPath { op: "checkout", path } if path == "alpha.txt"),
            "{err}"
        );
        assert!(
            err.to_string().starts_with("checkout: 'alpha.txt'"),
            "{err}"
        );
        assert!(!err.to_string().contains("apply the stash"), "{err}");
        assert_eq!(head_branch(&fx), "main", "HEAD stayed put");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"work in progress\n",
            "the local edit must survive"
        );
        assert!(
            !fx.path().join("feature.txt").exists(),
            "nothing was written"
        );
    }

    /// Stages `alpha.txt` with `mtime`, ignores ctime, then makes the file unreadable: only a
    /// check that never reads the content can settle it.
    fn unreadable_alpha(fx: &Fixture, mtime: std::time::SystemTime) {
        fx.set_mtime("alpha.txt", mtime);
        fx.git(&["add", "alpha.txt"]);
        fx.git(&["config", "core.trustctime", "false"]);
        fx.set_mode("alpha.txt", 0o000);
    }

    #[test]
    fn a_file_whose_size_differs_is_dirty_without_reading_it() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"a longer work in progress\n");
        fx.set_mode("alpha.txt", 0o000);

        let err = checkout_branch(fx.path(), &Policy::default(), "feature").unwrap_err();

        fx.set_mode("alpha.txt", 0o644);
        assert!(
            matches!(&err, ActionError::DirtyPath { op: "checkout", path } if path == "alpha.txt"),
            "{err}"
        );
        assert_eq!(head_branch(&fx), "main", "HEAD stayed put");
    }

    #[test]
    fn a_file_whose_stat_matches_the_index_is_clean_without_reading_it() {
        let fx = Fixture::standard();
        let an_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_hours(1);
        unreadable_alpha(&fx, an_hour_ago);

        checkout_branch(fx.path(), &Policy::default(), "feature").unwrap();

        assert_eq!(head_branch(&fx), "feature");
        assert!(!fx.path().join("alpha.txt").exists());
    }

    #[test]
    fn a_racy_index_entry_is_not_evidence_and_the_file_is_read() {
        let fx = Fixture::standard();
        let tomorrow = std::time::SystemTime::now() + std::time::Duration::from_hours(24);
        unreadable_alpha(&fx, tomorrow);

        let err = checkout_branch(fx.path(), &Policy::default(), "feature").unwrap_err();

        fx.set_mode("alpha.txt", 0o644);
        assert!(
            matches!(&err, ActionError::Git { .. }) && err.to_string().contains("alpha.txt"),
            "{err}"
        );
        assert_eq!(head_branch(&fx), "main", "HEAD stayed put");
    }

    #[test]
    fn an_untracked_file_the_target_branch_adds_refuses_the_switch() {
        let fx = Fixture::standard();
        fx.write("feature.txt", b"mine, not the branch's\n");

        let err = checkout_branch(fx.path(), &Policy::default(), "feature").unwrap_err();
        assert!(
            matches!(&err, ActionError::DirtyPath { path, .. } if path == "feature.txt"),
            "{err}"
        );
        assert_eq!(head_branch(&fx), "main");
        assert_eq!(
            std::fs::read(fx.path().join("feature.txt")).unwrap(),
            b"mine, not the branch's\n"
        );
    }

    #[test]
    fn an_untracked_file_where_the_target_needs_a_folder_refuses_the_switch() {
        let fx = Fixture::empty_dir();
        fx.git(&["init", "-q", "-b", "main"]);
        fx.write("0.txt", b"main\n");
        fx.commit("base", "2024-01-01T00:00:00+0000");
        fx.git(&["switch", "-q", "-c", "nested"]);
        fx.write("0.txt", b"nested\n");
        fx.write("a/b", b"b\n");
        fx.commit("nested", "2024-01-02T00:00:00+0000");
        fx.git(&["switch", "-q", "main"]);
        fx.write("a", b"untracked\n");

        let err = checkout_branch(fx.path(), &Policy::default(), "nested").unwrap_err();

        assert!(
            matches!(&err, ActionError::DirtyPath { op: "checkout", path } if path == "a"),
            "{err}"
        );
        assert_eq!(head_branch(&fx), "main", "HEAD stayed put");
        assert_eq!(
            std::fs::read(fx.path().join("0.txt")).unwrap(),
            b"main\n",
            "a path sorting before the blocked one was not written"
        );
        assert_eq!(std::fs::read(fx.path().join("a")).unwrap(), b"untracked\n");
        assert_eq!(porcelain(&fx), "?? a");
    }

    /// `main` tracks `a` as a file or (`link`) a symlink to `elsewhere`, and `nested` (from
    /// `main`) replaces it with the file `nested_path` below `a`. Checked out on `main`.
    fn replaced_by_nested_fixture(link: bool, nested_path: &str) -> Fixture {
        let fx = Fixture::empty_dir();
        fx.git(&["init", "-q", "-b", "main"]);
        fx.write("0.txt", b"main\n");
        fx.write("elsewhere/keep", b"k\n");
        if link {
            fx.symlink("elsewhere", "a");
        } else {
            fx.write("a", b"file\n");
        }
        fx.commit("base", "2024-01-01T00:00:00+0000");
        fx.git(&["switch", "-q", "-c", "nested"]);
        fx.remove("a");
        fx.write(nested_path, b"nested\n");
        fx.commit("nested", "2024-01-02T00:00:00+0000");
        fx.git(&["switch", "-q", "main"]);
        fx
    }

    /// The tracked entries and the working-tree files below `a`, as one comparable listing.
    #[expect(clippy::unwrap_used, reason = "test helper panics on fixture failure")]
    fn tree_and_disk(fx: &Fixture) -> (String, Vec<String>) {
        let index = fx.git_output(&["ls-files", "-s"]);
        let mut disk = Vec::new();
        let mut pending = vec![fx.path().join("a")];
        while let Some(path) = pending.pop() {
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(fx.path()).unwrap().display().to_string();
            if meta.is_dir() {
                pending.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            } else if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                disk.push(format!("{relative} -> {}", target.display()));
            } else {
                let data = std::fs::read(&path).unwrap();
                disk.push(format!("{relative}: {}", String::from_utf8_lossy(&data)));
            }
        }
        disk.sort();
        (index, disk)
    }

    #[test]
    fn a_switch_writes_below_a_file_or_symlink_it_deletes_like_git() {
        for (link, nested_path) in [(false, "a/b/c"), (true, "a/b")] {
            let fx = replaced_by_nested_fixture(link, nested_path);
            let by_git = replaced_by_nested_fixture(link, nested_path);
            by_git.git(&["switch", "-q", "nested"]);

            checkout_branch(fx.path(), &Policy::default(), "nested")
                .unwrap_or_else(|err| panic!("{nested_path}: {err}"));

            assert_eq!(head_branch(&fx), "nested", "{nested_path}");
            assert_eq!(porcelain(&fx), "", "{nested_path}");
            assert_eq!(tree_and_disk(&fx), tree_and_disk(&by_git), "{nested_path}");
            assert_eq!(
                std::fs::read(fx.path().join("elsewhere/keep")).unwrap(),
                b"k\n",
                "{nested_path}: the old link's target is left alone"
            );
            assert!(
                !fx.path().join("elsewhere/b").exists(),
                "{nested_path}: nothing was written through the old link"
            );

            checkout_branch(fx.path(), &Policy::default(), "main").unwrap();
            by_git.git(&["switch", "-q", "main"]);
            assert_eq!(porcelain(&fx), "", "{nested_path}: and back");
            assert_eq!(tree_and_disk(&fx), tree_and_disk(&by_git), "{nested_path}");
        }
    }

    /// `main` has `docs/guide.md`, `nodocs` (from `main`) deletes it, and `docsfile` (from
    /// `nodocs`) adds a file named `docs`. Checked out on `main`.
    fn docs_fixture() -> Fixture {
        let fx = Fixture::empty_dir();
        fx.git(&["init", "-q", "-b", "main"]);
        fx.write("keep", b"x\n");
        fx.write("docs/guide.md", b"g\n");
        fx.commit("base", "2024-01-01T00:00:00+0000");
        fx.git(&["switch", "-q", "-c", "nodocs"]);
        fx.remove("docs");
        fx.commit("rm", "2024-01-02T00:00:00+0000");
        fx.git(&["switch", "-q", "-c", "docsfile"]);
        fx.write("docs", b"f\n");
        fx.commit("file", "2024-01-03T00:00:00+0000");
        fx.git(&["switch", "-q", "main"]);
        fx
    }

    #[test]
    fn switching_away_removes_an_emptied_folder() {
        let fx = docs_fixture();

        checkout_branch(fx.path(), &Policy::default(), "nodocs").unwrap();

        assert!(
            !fx.path().join("docs").exists(),
            "a folder the switch empties is removed, as git removes it"
        );
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn switching_away_removes_every_emptied_level_and_keeps_the_work_dir() {
        let fx = Fixture::empty_dir();
        fx.git(&["init", "-q", "-b", "main"]);
        fx.write("top", b"t\n");
        fx.commit("base", "2024-01-01T00:00:00+0000");
        fx.git(&["switch", "-q", "-c", "deep"]);
        fx.write("sub/deep/x", b"x\n");
        fx.write("other/y", b"y\n");
        fx.commit("deep", "2024-01-02T00:00:00+0000");
        fx.write("other/untracked", b"mine\n");

        checkout_branch(fx.path(), &Policy::default(), "main").unwrap();

        assert!(
            !fx.path().join("sub").exists(),
            "sub/deep and sub both emptied"
        );
        assert!(
            fx.path().join("other/untracked").exists(),
            "a folder still holding an untracked file stays"
        );
        assert!(fx.path().join("top").exists());
    }

    #[test]
    fn a_folder_can_become_a_file() {
        let fx = docs_fixture();
        checkout_branch(fx.path(), &Policy::default(), "nodocs").unwrap();
        checkout_branch(fx.path(), &Policy::default(), "docsfile").unwrap();
        assert_eq!(std::fs::read(fx.path().join("docs")).unwrap(), b"f\n");
        assert_eq!(porcelain(&fx), "");

        let direct = docs_fixture();
        let outcome = checkout_branch(direct.path(), &Policy::default(), "docsfile").unwrap();
        assert_eq!(
            outcome.files_changed, 2,
            "docs/guide.md deleted, docs added"
        );
        assert_eq!(std::fs::read(direct.path().join("docs")).unwrap(), b"f\n");
        assert_eq!(
            porcelain(&direct),
            "",
            "the index must agree with the new tree"
        );

        checkout_branch(direct.path(), &Policy::default(), "main").unwrap();
        assert_eq!(
            std::fs::read(direct.path().join("docs/guide.md")).unwrap(),
            b"g\n",
            "and the file becomes a folder again"
        );
        assert_eq!(porcelain(&direct), "");
    }

    #[test]
    fn a_leftover_empty_folder_gives_way_to_a_file() {
        let fx = docs_fixture();
        fx.git(&["switch", "-q", "nodocs"]);
        fx.write("docs/placeholder", b"");
        fx.remove("docs/placeholder");

        checkout_branch(fx.path(), &Policy::default(), "docsfile").unwrap();

        assert_eq!(std::fs::read(fx.path().join("docs")).unwrap(), b"f\n");
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn a_folder_holding_untracked_work_is_not_replaced_by_a_file() {
        for extra in ["docs/notes.txt", "docs/empty/"] {
            let fx = docs_fixture();
            if let Some(dir) = extra.strip_suffix('/') {
                let placeholder = format!("{dir}/placeholder");
                fx.write(&placeholder, b"");
                fx.remove(&placeholder);
            } else {
                fx.write(extra, b"mine\n");
            }

            let err = checkout_branch(fx.path(), &Policy::default(), "docsfile").unwrap_err();

            assert!(
                matches!(&err, ActionError::DirtyPath { op: "checkout", path } if path == "docs"),
                "{extra}: {err}"
            );
            assert_eq!(head_branch(&fx), "main", "{extra}: HEAD stayed put");
            assert!(
                fx.path().join("docs/guide.md").exists(),
                "{extra}: nothing was deleted"
            );
            assert!(
                fx.path().join(extra).exists(),
                "{extra}: the untracked work stays"
            );
        }
    }

    #[test]
    fn an_edit_outside_the_switch_carries_over() {
        let fx = Fixture::standard();
        // README.md is identical on both branches, so the switch does not touch it.
        fx.write("README.md", b"# fixture\n\nkeep me\n");

        checkout_branch(fx.path(), &Policy::default(), "feature").unwrap();
        assert_eq!(head_branch(&fx), "feature");
        assert_eq!(
            std::fs::read(fx.path().join("README.md")).unwrap(),
            b"# fixture\n\nkeep me\n",
            "an edit the switch does not touch carries over, as git leaves it"
        );
    }

    #[test]
    fn a_detached_head_can_switch_onto_a_branch() {
        let fx = Fixture::standard();
        fx.git(&["checkout", "-q", "--detach", "HEAD"]);
        let detached = fx.rev_parse("HEAD");

        checkout_branch(fx.path(), &Policy::default(), "feature").unwrap();
        assert_eq!(head_branch(&fx), "feature");
        assert_eq!(fx.rev_parse("HEAD"), fx.rev_parse("feature"));
        assert_eq!(porcelain(&fx), "");
        assert_eq!(
            fx.git_output(&["reflog", "show", "--format=%gs", "-1", "HEAD"]),
            format!("checkout: moving from {detached} to feature"),
            "a detached start is logged by its full id, which `git checkout -` parses"
        );

        fx.git(&["checkout", "-q", "-"]);
        assert_eq!(fx.rev_parse("HEAD"), detached);
        assert_eq!(
            head_branch(&fx),
            "HEAD",
            "`git checkout -` lands detached again"
        );
    }

    #[test]
    fn a_switch_does_not_create_a_missing_head_reflog() {
        let fx = Fixture::standard();
        let reflog = fx.git_dir().join("logs/HEAD");
        std::fs::remove_file(&reflog).unwrap();

        checkout_branch(fx.path(), &Policy::default(), "feature").unwrap();
        assert_eq!(head_branch(&fx), "feature");
        assert!(
            !reflog.exists(),
            "a repository without logs/HEAD must not gain one from a switch"
        );
    }

    #[test]
    fn a_switch_from_an_unborn_branch_logs_its_name_and_a_null_old_id() {
        let fx = Fixture::standard();
        fx.git(&["checkout", "-q", "--orphan", "none"]);
        fx.git(&["rm", "-rf", "-q", "."]);

        checkout_branch(fx.path(), &Policy::default(), "main").unwrap();
        assert_eq!(head_branch(&fx), "main");
        assert_eq!(
            fx.git_output(&["reflog", "show", "--format=%gs", "-1", "HEAD"]),
            "checkout: moving from none to main"
        );
        let log = std::fs::read_to_string(fx.git_dir().join("logs/HEAD")).unwrap();
        let last = log.lines().last().unwrap();
        assert!(
            last.starts_with(&"0".repeat(40)),
            "an unborn start has no old commit, so the old id is null: {last}"
        );
    }

    #[test]
    fn checkout_refuses_a_tree_path_under_dot_git() {
        for path in [
            ".git/hooks/pre-commit",
            ".GIT/hooks/pre-commit",
            ".git/probe.txt",
            ".GIT/probe.txt",
        ] {
            let fx = Fixture::standard();
            let head = fx.rev_parse("HEAD");
            let evil = fx.commit_raw_path(path, b"#!/bin/sh\necho hooked\n", "100755");
            fx.git(&["branch", "evil", &evil]);

            let err = checkout_branch(fx.path(), &Policy::default(), "evil").unwrap_err();
            assert!(
                matches!(&err, ActionError::Git { op: "checkout", name, .. } if name == path),
                "{path}: {err}"
            );
            assert!(!fx.git_dir().join("hooks/pre-commit").exists(), "{path}");
            assert!(!fx.git_dir().join("probe.txt").exists(), "{path}");
            assert_eq!(head_branch(&fx), "main", "{path}");
            assert_eq!(fx.rev_parse("HEAD"), head, "{path}");
            assert_eq!(porcelain(&fx), "", "{path}");
        }
    }

    #[expect(clippy::unwrap_used, reason = "test helper panics on fixture failure")]
    fn outside_is_empty(outside: &Fixture) -> bool {
        std::fs::read_dir(outside.path()).unwrap().next().is_none()
    }

    #[test]
    fn an_untracked_symlinked_directory_is_not_written_through() {
        let fx = Fixture::standard();
        let head = fx.rev_parse("HEAD");
        let vendored = fx.commit_raw_path("vendor/lib.rs", b"lib\n", "100644");
        fx.git(&["branch", "vendored", &vendored]);
        let outside = Fixture::empty_dir();
        fx.symlink(outside.path().to_str().unwrap(), "vendor");

        let err = checkout_branch(fx.path(), &Policy::default(), "vendored").unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "checkout", name, .. } if name == "vendor/lib.rs"),
            "{err}"
        );
        assert!(err.to_string().contains(CROSSES_LINK), "{err}");
        assert!(outside_is_empty(&outside));
        assert_eq!(head_branch(&fx), "main");
        assert_eq!(fx.rev_parse("HEAD"), head);
    }

    /// A tree holding a symlink and a directory that differ only in case is valid to git,
    /// but on a case-insensitive volume writing the file after the link follows the link.
    /// The plan is refused whatever the volume, before anything is written.
    #[test]
    fn a_symlink_that_case_folds_onto_a_directory_cannot_write_outside_the_tree() {
        for (link, file) in [("D", "d/payload"), ("Z", "z/p"), ("Dir", "dir/pwned.txt")] {
            let fx = Fixture::standard();
            let head = fx.rev_parse("HEAD");
            let outside = Fixture::empty_dir();
            let target = outside.path().to_str().unwrap().as_bytes();
            let evil =
                fx.commit_raw_entries(&[(link, target, "120000"), (file, b"pwned\n", "100644")]);
            fx.git(&["branch", "evil", &evil]);

            let err = checkout_branch(fx.path(), &Policy::default(), "evil").unwrap_err();
            assert!(
                matches!(&err, ActionError::Git { op: "checkout", name, .. } if name == file),
                "{link}: {err}"
            );
            assert!(err.to_string().contains(CROSSES_LINK), "{link}: {err}");
            assert!(outside_is_empty(&outside), "{link} wrote outside the tree");
            assert!(
                std::fs::symlink_metadata(fx.path().join(link)).is_err(),
                "{link} was written before the refusal"
            );
            assert_eq!(head_branch(&fx), "main", "{link}");
            assert_eq!(fx.rev_parse("HEAD"), head, "{link}");
            assert_eq!(porcelain(&fx), "", "{link}");
        }
    }

    #[test]
    fn a_case_only_rename_switches_like_git() {
        let fx = Fixture::standard();
        fx.git(&["config", "core.ignorecase", "true"]);
        fx.git(&["checkout", "-q", "-b", "renamed"]);
        fx.git(&["mv", "README.md", "Readme.md"]);
        fx.git(&["commit", "-qm", "case-only rename"]);
        fx.git(&["checkout", "-q", "main"]);
        let renamed_tip = fx.rev_parse("renamed");

        let outcome = checkout_branch(fx.path(), &Policy::default(), "renamed").unwrap();
        assert_eq!(outcome.new_head.to_string(), renamed_tip);
        let names: Vec<String> = std::fs::read_dir(fx.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"Readme.md".to_owned()), "{names:?}");
        assert!(!names.contains(&"README.md".to_owned()), "{names:?}");
        assert_eq!(
            porcelain(&fx),
            "",
            "the index and the disk agree with the new tree"
        );

        checkout_branch(fx.path(), &Policy::default(), "main").unwrap();
        assert_eq!(porcelain(&fx), "");
        assert!(fx.path().join("README.md").exists());
    }

    #[test]
    fn a_case_variant_branch_name_is_not_checked_out() {
        let fx = Fixture::standard();
        fx.git(&["config", "core.ignorecase", "true"]);
        let err = checkout_branch(fx.path(), &Policy::default(), "FEATURE").unwrap_err();
        assert!(err.to_string().contains("refs/heads/FEATURE"), "{err}");
        assert_eq!(head_branch(&fx), "main");
    }

    #[test]
    fn a_branch_that_does_not_exist_locally_is_reported_by_name() {
        let fx = Fixture::standard();
        let err = checkout_branch(fx.path(), &Policy::default(), "master").unwrap_err();
        assert!(matches!(err, ActionError::Git { .. }), "{err}");
        assert!(err.to_string().contains("refs/heads/master"), "{err}");
        assert_eq!(head_branch(&fx), "main");
    }

    #[test]
    fn a_switch_that_turns_a_symlink_into_a_file_succeeds() {
        let fx = Fixture::standard();
        fx.git(&["branch", "plain"]);
        fx.git(&["switch", "-q", "plain"]);
        fx.remove("link");
        fx.write("link", b"plain file\n");
        fx.commit("Replace link with a file", "2024-02-01T00:00:00+0000");
        fx.git(&["switch", "-q", "main"]);
        assert_eq!(porcelain(&fx), "", "the tree is clean before the switch");

        checkout_branch(fx.path(), &Policy::default(), "plain").unwrap();
        let link = fx.path().join("link");
        assert!(std::fs::symlink_metadata(&link).unwrap().is_file());
        assert_eq!(std::fs::read(&link).unwrap(), b"plain file\n");
        assert_eq!(fx.rev_parse("HEAD"), fx.rev_parse("plain"));
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn a_switch_that_turns_a_file_into_a_symlink_succeeds() {
        let fx = Fixture::standard();
        fx.git(&["branch", "linked"]);
        fx.git(&["switch", "-q", "linked"]);
        fx.remove("alpha.txt");
        fx.symlink("README.md", "alpha.txt");
        fx.commit("Replace alpha with a link", "2024-02-01T00:00:00+0000");
        fx.git(&["switch", "-q", "main"]);
        assert_eq!(porcelain(&fx), "", "the tree is clean before the switch");

        checkout_branch(fx.path(), &Policy::default(), "linked").unwrap();
        assert_eq!(
            std::fs::read_link(fx.path().join("alpha.txt")).unwrap(),
            std::path::Path::new("README.md")
        );
        assert_eq!(fx.rev_parse("HEAD"), fx.rev_parse("linked"));
        assert_eq!(porcelain(&fx), "");
    }
}

/// Checkout and fast-forward pull read a touched path the way git hashes it: the kind and the
/// executable bit count, and a read failure is reported as itself.
mod tree_switch_disk_reads {
    use rc_git_guard::Policy;
    use rc_git_guard::actions::{ActionError, checkout_branch, pull_fast_forward};
    use rc_git_testkit::Fixture;

    /// `main` as the standard fixture has it, and `next` (also `origin/main`) one commit
    /// ahead with `alpha.txt` rewritten, so both a switch and a pull touch that file.
    fn alpha_rewritten_ahead() -> Fixture {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"alpha\nmore\n");
        fx.commit("Rewrite alpha", "2024-02-01T00:00:00+0000");
        fx.git(&["branch", "next"]);
        fx.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        fx.git(&["reset", "-q", "--hard", "HEAD~1"]);
        // Pull reads only the tracking ref; the remote just has to exist. Nothing is fetched.
        let url = fx.path().to_string_lossy().into_owned();
        fx.git(&["remote", "add", "origin", &url]);
        fx
    }

    fn mode(fx: &Fixture, relative: &str) -> Option<u32> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(fx.path().join(relative))
            .ok()
            .map(|meta| meta.permissions().mode() & 0o777)
    }

    fn assert_alpha_dirty(err: &ActionError, expected_op: &str) {
        assert!(
            matches!(err, ActionError::DirtyPath { op, path } if *op == expected_op && path == "alpha.txt"),
            "{err}"
        );
    }

    #[test]
    fn an_uncommitted_chmod_refuses_the_switch_and_the_pull() {
        let fx = alpha_rewritten_ahead();
        fx.set_mode("alpha.txt", 0o755);
        assert!(
            fx.git_output(&["status", "--porcelain"])
                .contains("M alpha.txt")
        );

        let main_tip = fx.rev_parse("main");
        let err = checkout_branch(fx.path(), &Policy::default(), "next").unwrap_err();
        assert_alpha_dirty(&err, "checkout");
        let err = pull_fast_forward(fx.path(), &Policy::default()).unwrap_err();
        assert_alpha_dirty(&err, "pull");
        assert_eq!(fx.rev_parse("HEAD"), main_tip, "HEAD stayed put");
        assert_eq!(mode(&fx, "alpha.txt"), Some(0o755), "the exec bit survives");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n"
        );
    }

    fn assert_index_locked(err: &ActionError, expected_op: &str) {
        assert!(
            matches!(err, ActionError::Git { op, name, .. } if *op == expected_op && name == "index"),
            "{err}"
        );
        assert!(
            err.to_string()
                .contains("the index is locked by another git process; retry"),
            "{err}"
        );
    }

    #[test]
    fn a_held_index_lock_refuses_the_switch_and_the_pull_before_any_file_is_written() {
        let fx = alpha_rewritten_ahead();
        let lock = fx.path().join(".git/index.lock");
        fx.write(".git/index.lock", b"");

        let main_tip = fx.rev_parse("main");
        let err = checkout_branch(fx.path(), &Policy::default(), "next").unwrap_err();
        assert_index_locked(&err, "checkout");
        let err = pull_fast_forward(fx.path(), &Policy::default()).unwrap_err();
        assert_index_locked(&err, "pull");
        assert!(lock.exists(), "another process's lock is never removed");
        assert_eq!(fx.rev_parse("HEAD"), main_tip, "HEAD stayed put");
        assert_eq!(
            fx.git_output(&["rev-parse", "--abbrev-ref", "HEAD"]),
            "main"
        );
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n",
            "no file was rewritten"
        );

        fx.remove(".git/index.lock");
        checkout_branch(fx.path(), &Policy::default(), "next").unwrap();
        assert!(!lock.exists(), "the switch releases the lock it took");
        assert_eq!(fx.git_output(&["status", "--porcelain"]), "");
    }

    #[test]
    fn a_chmod_is_no_change_when_the_repository_ignores_file_modes() {
        let fx = alpha_rewritten_ahead();
        fx.git(&["config", "core.fileMode", "false"]);
        fx.set_mode("alpha.txt", 0o755);
        assert_eq!(fx.git_output(&["status", "--porcelain"]), "");

        checkout_branch(fx.path(), &Policy::default(), "next").unwrap();
        assert_eq!(fx.rev_parse("HEAD"), fx.rev_parse("next"));
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\nmore\n"
        );
    }

    #[test]
    fn a_staged_chmod_on_a_touched_path_refuses_the_switch_and_the_pull() {
        let fx = alpha_rewritten_ahead();
        fx.git(&["config", "core.fileMode", "false"]);
        fx.git(&["update-index", "--chmod=+x", "alpha.txt"]);
        assert_eq!(fx.git_output(&["status", "--porcelain"]), "M  alpha.txt");

        let main_tip = fx.rev_parse("main");
        let err = checkout_branch(fx.path(), &Policy::default(), "next").unwrap_err();
        assert_alpha_dirty(&err, "checkout");
        let err = pull_fast_forward(fx.path(), &Policy::default()).unwrap_err();
        assert_alpha_dirty(&err, "pull");
        assert_eq!(fx.rev_parse("HEAD"), main_tip, "HEAD stayed put");
        assert!(
            fx.git_output(&["ls-files", "-s", "alpha.txt"])
                .starts_with("100755 "),
            "the staged mode survives"
        );
    }

    #[test]
    fn a_file_swapped_for_a_symlink_to_identical_bytes_refuses_the_switch_and_the_pull() {
        let fx = alpha_rewritten_ahead();
        fx.write("copy.txt", b"alpha\n");
        fx.remove("alpha.txt");
        fx.symlink("copy.txt", "alpha.txt");
        assert!(
            fx.git_output(&["status", "--porcelain"])
                .contains("T alpha.txt")
        );

        let main_tip = fx.rev_parse("main");
        let err = checkout_branch(fx.path(), &Policy::default(), "next").unwrap_err();
        assert_alpha_dirty(&err, "checkout");
        let err = pull_fast_forward(fx.path(), &Policy::default()).unwrap_err();
        assert_alpha_dirty(&err, "pull");
        assert_eq!(fx.rev_parse("HEAD"), main_tip, "HEAD stayed put");
        assert_eq!(
            std::fs::read_link(fx.path().join("alpha.txt")).unwrap(),
            std::path::Path::new("copy.txt"),
            "the symlink survives"
        );
    }

    #[test]
    fn an_unreadable_tracked_file_reports_the_os_error_not_a_dirty_path() {
        let fx = alpha_rewritten_ahead();
        fx.set_mode("alpha.txt", 0o000);
        if std::fs::read(fx.path().join("alpha.txt")).is_ok() {
            // Running as root: mode 000 does not stop the read, so there is no error to see.
            return;
        }

        let err = checkout_branch(fx.path(), &Policy::default(), "next").unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "checkout", name, .. } if name == "alpha.txt"),
            "{err}"
        );
        assert!(err.to_string().contains("ermission denied"), "{err}");
        let err = pull_fast_forward(fx.path(), &Policy::default()).unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "pull", name, .. } if name == "alpha.txt"),
            "{err}"
        );
        fx.set_mode("alpha.txt", 0o644);
        assert_eq!(fx.rev_parse("HEAD"), fx.rev_parse("main"));
    }

    #[test]
    fn a_clean_switch_onto_a_committed_chmod_sets_the_exec_bit() {
        let fx = Fixture::standard();
        fx.set_mode("alpha.txt", 0o755);
        fx.commit("Make alpha executable", "2024-02-01T00:00:00+0000");
        fx.git(&["branch", "exec"]);
        fx.git(&["reset", "-q", "--hard", "HEAD~1"]);
        assert_eq!(mode(&fx, "alpha.txt"), Some(0o644));

        checkout_branch(fx.path(), &Policy::default(), "exec").unwrap();
        assert_eq!(mode(&fx, "alpha.txt"), Some(0o755));
        assert_eq!(fx.git_output(&["status", "--porcelain"]), "");

        checkout_branch(fx.path(), &Policy::default(), "main").unwrap();
        assert_eq!(mode(&fx, "alpha.txt"), Some(0o644));
        assert_eq!(fx.git_output(&["status", "--porcelain"]), "");
    }
}

/// A merge, cherry-pick or other git operation left in progress refuses every action that would
/// build on it or move `HEAD`, and a conflicted index refuses a switch on its own.
mod in_progress {
    use rc_git_guard::Policy;
    use rc_git_guard::actions::{
        ActionError, CommitRequest, FileSelection, StashRequest, checkout_branch, commit,
        stash_push,
    };
    use rc_git_testkit::Fixture;

    use super::explicit;

    #[expect(
        clippy::unwrap_used,
        reason = "test helper panics on a malformed HEAD id"
    )]
    fn head_oid(fx: &Fixture) -> rc_git_core::Oid {
        rc_git_core::Oid::from_hex(&fx.rev_parse("HEAD")).unwrap()
    }

    fn commit_readme(fx: &Fixture) -> Result<rc_git_guard::actions::Committed, ActionError> {
        commit(
            fx.path(),
            &Policy::default(),
            CommitRequest {
                message: "resolved",
                author: explicit(),
                expected_head: Some(head_oid(fx)),
                selections: &[FileSelection::Whole {
                    path: "README.md".to_owned(),
                }],
            },
            None,
        )
    }

    fn assert_refused(err: &ActionError, op: &str, what: &str) {
        assert!(
            matches!(err, ActionError::Git { op: actual, .. } if *actual == op),
            "{err}"
        );
        assert!(
            err.to_string().contains(&format!(
                "a {what} is in progress; finish or abort it in git"
            )),
            "{err}"
        );
    }

    #[test]
    fn a_commit_during_a_conflicted_merge_is_refused_and_head_stays() {
        let fx = Fixture::standard();
        fx.conflicted_merge("README.md");
        let head = fx.rev_parse("HEAD");
        fx.write("README.md", b"resolved\n");

        let err = commit_readme(&fx).unwrap_err();
        assert_refused(&err, "commit", "merge");
        assert_eq!(fx.rev_parse("HEAD"), head);
        assert!(fx.git_dir().join("MERGE_HEAD").is_file());
    }

    #[test]
    fn a_commit_during_a_cherry_pick_is_refused() {
        let fx = Fixture::standard();
        fx.conflicted_merge("README.md");
        fx.git(&["merge", "--abort"]);
        let report = fx.git_expect_failure(&["cherry-pick", "clash"]);
        assert!(report.contains("CONFLICT"), "{report}");
        let head = fx.rev_parse("HEAD");
        fx.write("README.md", b"resolved\n");

        let err = commit_readme(&fx).unwrap_err();
        assert_refused(&err, "commit", "cherry-pick");
        assert_eq!(fx.rev_parse("HEAD"), head);
    }

    #[test]
    fn a_commit_during_a_bisect_goes_through() {
        let fx = Fixture::standard();
        fx.git(&["bisect", "start"]);
        assert!(fx.git_dir().join("BISECT_LOG").is_file());
        fx.write("README.md", b"found it\n");

        let done = commit_readme(&fx).unwrap();
        assert_eq!(done.oid.to_hex(), fx.rev_parse("HEAD"));
    }

    #[test]
    fn a_stash_during_a_conflicted_merge_is_refused() {
        let fx = Fixture::standard();
        fx.conflicted_merge("README.md");
        fx.write("alpha.txt", b"side work\n");

        let err = stash_push(
            fx.path(),
            &Policy::default(),
            StashRequest {
                paths: &["alpha.txt".to_owned()],
                message: None,
                author: explicit(),
            },
            None,
        )
        .unwrap_err();
        assert_refused(&err, "stash push", "merge");
        assert_eq!(fx.git_output(&["stash", "list"]), "");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"side work\n"
        );
    }

    #[test]
    fn a_checkout_during_a_conflicted_merge_is_refused_and_head_stays() {
        let fx = Fixture::standard();
        fx.conflicted_merge("README.md");
        let head = fx.rev_parse("HEAD");

        let err = checkout_branch(fx.path(), &Policy::default(), "feature").unwrap_err();
        assert_refused(&err, "checkout", "merge");
        assert_eq!(fx.git_output(&["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(fx.rev_parse("HEAD"), head);
        assert!(
            !fx.path().join("feature.txt").exists(),
            "nothing was written"
        );
    }

    #[test]
    fn a_checkout_during_a_bisect_is_refused() {
        let fx = Fixture::standard();
        fx.git(&["bisect", "start"]);

        let err = checkout_branch(fx.path(), &Policy::default(), "feature").unwrap_err();
        assert_refused(&err, "checkout", "bisect");
        assert_eq!(fx.git_output(&["symbolic-ref", "--short", "HEAD"]), "main");
    }

    #[test]
    fn a_conflicted_path_the_switch_does_not_touch_still_refuses_it() {
        let fx = Fixture::standard();
        // README.md is identical on main and feature, so only the index stages show the conflict.
        fx.conflicted_merge("README.md");
        fx.remove(".git/MERGE_HEAD");

        let err = checkout_branch(fx.path(), &Policy::default(), "feature").unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "checkout", name, .. } if name == "README.md"),
            "{err}"
        );
        assert!(err.to_string().contains("unresolved conflicts"), "{err}");
        assert_eq!(fx.git_output(&["symbolic-ref", "--short", "HEAD"]), "main");
        assert!(
            !fx.path().join("feature.txt").exists(),
            "nothing was written"
        );
    }
}

mod partial_writes {
    //! A write that fails part way through a multi-path action names how far it got, so the
    //! user learns which earlier paths already changed and what was never updated.

    use rc_git_guard::Policy;
    use rc_git_guard::actions::{
        ActionError, StashRequest, checkout_branch, discard_files, stash_apply, stash_push,
    };
    use rc_git_testkit::Fixture;

    use super::explicit;

    /// Makes `src` read-only, so a write to `alpha.txt` succeeds and one to `src/lib.rs`
    /// fails. False when running as root, where the mode does not stop the write.
    fn make_src_read_only(fx: &Fixture) -> bool {
        fx.set_mode("src", 0o555);
        #[expect(
            clippy::disallowed_methods,
            reason = "probes whether the read-only folder stops a write in this environment"
        )]
        let probe = std::fs::write(fx.path().join("src/probe"), b"");
        if probe.is_ok() {
            fx.set_mode("src", 0o755);
            return false;
        }
        true
    }

    fn dirty_both(fx: &Fixture) {
        fx.write("alpha.txt", b"dirty\n");
        fx.write("src/lib.rs", b"dirty\n");
    }

    fn paths() -> Vec<String> {
        vec!["alpha.txt".to_owned(), "src/lib.rs".to_owned()]
    }

    fn assert_stopped(err: &ActionError, op: &str, unchanged: &str) {
        assert!(
            matches!(err, ActionError::Git { op: got, name, .. } if *got == op && name == "src/lib.rs"),
            "{err}"
        );
        let message = err.to_string();
        assert!(message.contains("ermission denied"), "{message}");
        assert!(
            message.contains(&format!("stopped after writing 1 of 2 paths; {unchanged}")),
            "{message}"
        );
    }

    #[test]
    fn a_discard_that_fails_part_way_says_what_it_already_wrote() {
        let fx = Fixture::standard();
        dirty_both(&fx);
        if !make_src_read_only(&fx) {
            return;
        }
        let err = discard_files(fx.path(), &Policy::default(), &paths(), None).unwrap_err();
        fx.set_mode("src", 0o755);

        assert_stopped(&err, "discard", "the index was not updated");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n",
            "the first path was already restored"
        );
    }

    #[test]
    fn a_stash_push_that_fails_part_way_says_the_stash_was_saved() {
        let fx = Fixture::standard();
        dirty_both(&fx);
        if !make_src_read_only(&fx) {
            return;
        }
        let err = stash_push(
            fx.path(),
            &Policy::default(),
            StashRequest {
                paths: &paths(),
                message: None,
                author: explicit(),
            },
            None,
        )
        .unwrap_err();
        fx.set_mode("src", 0o755);

        assert_stopped(
            &err,
            "stash push",
            "the stash holds every change, but the index was not updated",
        );
        assert_eq!(
            fx.git_output(&["show", "stash@{0}:src/lib.rs"]),
            "dirty",
            "the stash was recorded before the restore"
        );
    }

    #[test]
    fn a_stash_apply_that_fails_part_way_says_the_entry_is_kept() {
        let fx = Fixture::standard();
        dirty_both(&fx);
        fx.git(&["stash", "push", "-q"]);
        let stash = rc_git_core::Oid::from_hex(&fx.rev_parse("stash@{0}")).unwrap();
        if !make_src_read_only(&fx) {
            return;
        }

        let err = stash_apply(fx.path(), &Policy::default(), 0, stash, None).unwrap_err();
        fx.set_mode("src", 0o755);

        assert_stopped(&err, "stash apply", "the stash entry is kept");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"dirty\n",
            "the first path was already written"
        );
        assert_eq!(
            fx.rev_parse("stash@{0}"),
            stash.to_string(),
            "the entry is kept"
        );
    }

    #[test]
    fn a_switch_that_fails_part_way_says_head_and_the_index_stayed() {
        let fx = Fixture::standard();
        let main_tip = fx.rev_parse("HEAD");
        fx.write("alpha.txt", b"next\n");
        fx.write("src/lib.rs", b"next\n");
        fx.commit("Rewrite two files", "2024-02-01T00:00:00+0000");
        fx.git(&["branch", "next"]);
        fx.git(&["reset", "-q", "--hard", "HEAD~1"]);
        if !make_src_read_only(&fx) {
            return;
        }

        let err = checkout_branch(fx.path(), &Policy::default(), "next").unwrap_err();
        fx.set_mode("src", 0o755);

        assert_stopped(&err, "checkout", "HEAD and the index were not updated");
        assert_eq!(fx.rev_parse("HEAD"), main_tip, "HEAD stayed put");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"next\n",
            "the first path was already written"
        );
    }
}
