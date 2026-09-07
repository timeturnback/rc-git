//! The write path: branch and tag creation and deletion, gated by the policy, with backup
//! refs proving the safety net works.

use rc_git_guard::actions::{
    ActionError, create_branch, create_tag, delete_branch, delete_tag, plan_branch_delete,
};
use rc_git_guard::{Decision, Policy, Risk};
use rc_git_testkit::Fixture;

fn refs(fx: &Fixture) -> String {
    fx.git_output(&["for-each-ref", "--format=%(refname)"])
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
fn deleting_a_merged_branch_needs_only_a_plain_confirm_and_no_backup() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    create_branch(fx.path(), &policy, "old-work", "main~1").unwrap();

    let plan = plan_branch_delete(fx.path(), &policy, "old-work").unwrap();
    assert!(plan.merged, "an ancestor of main is merged");
    let Decision::Confirm(challenge) = &plan.decision else {
        panic!("expected Confirm, got {:?}", plan.decision);
    };
    assert_eq!(challenge.risk, Risk::Recoverable);
    assert!(challenge.typed_phrase.is_none());
    assert!(!challenge.backup_ref);

    let deleted = delete_branch(fx.path(), &policy, "old-work", None).unwrap();
    assert_eq!(deleted.full_name, "refs/heads/old-work");
    assert!(deleted.backup_ref.is_none());
    assert!(!refs(&fx).contains("refs/heads/old-work"));
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
    assert_eq!(challenge.typed_phrase.as_deref(), Some("feature"));
    assert!(challenge.backup_ref);

    let err = delete_branch(fx.path(), &policy, "feature", Some("wrong")).unwrap_err();
    assert!(matches!(err, ActionError::NotConfirmed { .. }), "{err}");
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
fn tag_deletion_demands_the_name_and_backs_up_the_tag_object() {
    let fx = Fixture::standard();
    let policy = Policy::default();
    let tag_object = fx.rev_parse("v1.0");

    let err = delete_tag(fx.path(), &policy, "v1.0", None).unwrap_err();
    assert!(matches!(err, ActionError::NotConfirmed { .. }), "{err}");

    let deleted = delete_tag(fx.path(), &policy, "v1.0", Some(" v1.0 ")).unwrap();
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

    let err = delete_tag(fx.path(), &policy, "nope", Some("nope")).unwrap_err();
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
}
