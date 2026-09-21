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

mod commit_and_discard {
    #![expect(
        clippy::disallowed_methods,
        reason = "tests arrange the fixture's working tree; only the guard mutates the repository"
    )]

    use rc_git_core::{BlobDiff, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT, Repository};
    use rc_git_guard::Policy;
    use rc_git_guard::actions::{
        ActionError, FileSelection, HunkStamp, Identity, commit, discard_files, discard_hunks,
    };
    use rc_git_testkit::Fixture;

    fn identity() -> Identity {
        Identity {
            name: "Test User".to_owned(),
            email: "test@example.com".to_owned(),
        }
    }

    fn whole(path: &str) -> FileSelection {
        FileSelection::Whole {
            path: path.to_owned(),
        }
    }

    fn porcelain(fx: &Fixture) -> String {
        fx.git_output(&["status", "--porcelain"])
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
        hunks
            .iter()
            .map(|hunk| HunkStamp {
                old_start: hunk.old_start,
                old_lines: hunk.old_lines,
                new_start: hunk.new_start,
                new_lines: hunk.new_lines,
            })
            .collect()
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
            "  Commit from rc-git  ",
            Some(identity()),
            Some(&head),
            &[
                whole("alpha.txt"),
                whole("brand-new.txt"),
                whole("Zeta.txt"),
            ],
            None,
        )
        .unwrap();
        assert_eq!(done.branch, "main");
        assert_eq!(done.oid, fx.rev_parse("HEAD"));

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

        let found = stamps(&fx, "hunky.txt");
        assert_eq!(found.len(), 2, "two separated edits make two hunks");
        let head = fx.rev_parse("HEAD");
        commit(
            fx.path(),
            &policy,
            "Take the first hunk",
            Some(identity()),
            Some(&head),
            &[FileSelection::Hunks {
                path: "hunky.txt".to_owned(),
                hunks: vec![found[0]],
            }],
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
            "First commit",
            Some(identity()),
            None,
            &[whole("first.txt")],
            None,
        )
        .unwrap();
        assert_eq!(done.branch, "main");
        assert_eq!(fx.git_output(&["log", "--format=%s"]), "First commit");
        assert_eq!(porcelain(&fx), "");
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
            "msg",
            Some(identity()),
            Some(&fx.rev_parse("HEAD~1")),
            &[whole("alpha.txt")],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::StaleRepository { .. }), "{err}");

        let err = commit(
            fx.path(),
            &policy,
            "msg",
            None,
            Some(&head),
            &[whole("alpha.txt")],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::MissingIdentity), "{err}");

        let err = commit(
            fx.path(),
            &policy,
            "   ",
            Some(identity()),
            Some(&head),
            &[whole("alpha.txt")],
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("message is empty"), "{err}");

        fx.git(&["checkout", "-q", "--detach", "HEAD"]);
        let err = commit(
            fx.path(),
            &policy,
            "msg",
            Some(identity()),
            Some(&head),
            &[whole("alpha.txt")],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::DetachedHead { .. }), "{err}");
        assert_eq!(fx.rev_parse("main"), head, "nothing moved");
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
            "Identity from config",
            None,
            Some(&head),
            &[whole("alpha.txt")],
            None,
        )
        .unwrap();
        assert_eq!(
            fx.git_output(&["log", "-1", "--format=%an|%ae"]),
            "Local Config|local@example.com"
        );
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
            "msg",
            Some(identity()),
            Some(&head),
            &[FileSelection::Hunks {
                path: "alpha.txt".to_owned(),
                hunks: vec![HunkStamp {
                    old_start: 99,
                    old_lines: 1,
                    new_start: 99,
                    new_lines: 1,
                }],
            }],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::HunkMismatch { .. }), "{err}");
        assert_eq!(fx.rev_parse("HEAD"), head, "nothing moved");
    }

    #[test]
    fn discard_restores_tracked_files_and_deletes_untracked() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"dirty\n");
        fx.write("untracked.txt", b"junk\n");

        discard_files(
            fx.path(),
            &policy,
            &["alpha.txt".to_owned(), "untracked.txt".to_owned()],
            None,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"alpha\n"
        );
        assert!(!fx.path().join("untracked.txt").exists());
        assert_eq!(porcelain(&fx), "");
    }

    #[test]
    fn discard_restores_a_symlink_and_the_exec_bit() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        std::fs::remove_file(fx.path().join("link")).unwrap();
        fx.write("link", b"now a plain file\n");

        discard_files(fx.path(), &policy, &["link".to_owned()], None).unwrap();
        assert_eq!(
            std::fs::read_link(fx.path().join("link")).unwrap().to_str(),
            Some("README.md")
        );
        assert_eq!(porcelain(&fx), "");
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
}

mod stash {
    #![expect(
        clippy::disallowed_methods,
        reason = "tests arrange the fixture's working tree; only the guard mutates the repository"
    )]

    use rc_git_guard::Policy;
    use rc_git_guard::actions::{
        ActionError, Identity, stash_apply, stash_drop, stash_pop, stash_push,
    };
    use rc_git_testkit::Fixture;

    fn identity() -> Identity {
        Identity {
            name: "Test User".to_owned(),
            email: "test@example.com".to_owned(),
        }
    }

    fn stash_list(fx: &Fixture) -> String {
        fx.git_output(&["stash", "list", "--format=%gs"])
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
            &["alpha.txt".to_owned(), "brand-new.txt".to_owned()],
            Some("half-done work"),
            Some(identity()),
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
        assert_eq!(fx.git_output(&["status", "--porcelain"]), "");

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
    fn our_pop_restores_a_stash_the_real_git_pushed() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"from real git\n");
        fx.git(&["stash", "push", "-q", "-m", "real one"]);

        let deleted = stash_pop(fx.path(), &policy, 0, None).unwrap();
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
            fx.git_output(&["status", "--porcelain"]),
            "M alpha.txt".to_owned(),
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

        let err = stash_drop(fx.path(), &policy, 1, Some("wrong")).unwrap_err();
        assert!(matches!(err, ActionError::NotConfirmed { .. }), "{err}");
        assert_eq!(
            stash_list(&fx),
            "On main: three\nOn main: two\nOn main: one"
        );

        let deleted = stash_drop(fx.path(), &policy, 1, Some("drop")).unwrap();
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

        stash_drop(fx.path(), &policy, 0, Some("drop")).unwrap();
        assert_eq!(stash_list(&fx), "On main: one");
        assert_eq!(fx.rev_parse("refs/stash"), one);

        stash_drop(fx.path(), &policy, 0, Some("drop")).unwrap();
        assert_eq!(stash_list(&fx), "");
        assert!(
            !fx.git_output(&["for-each-ref", "--format=%(refname)"])
                .contains("refs/stash")
        );
    }

    #[test]
    fn apply_refuses_a_dirty_path_and_touches_nothing() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"stash me\n");
        fx.git(&["stash", "push", "-q", "-m", "kept"]);
        fx.write("alpha.txt", b"conflicting edit\n");

        let err = stash_apply(fx.path(), &policy, 0, None).unwrap_err();
        assert!(matches!(err, ActionError::DirtyPath { .. }), "{err}");
        assert!(err.to_string().contains("alpha.txt"), "{err}");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"conflicting edit\n",
            "nothing was written"
        );
        assert_eq!(stash_list(&fx), "On main: kept", "the stash survives");

        // Clean state again: apply works and keeps the entry.
        fx.git(&["checkout", "-q", "--", "alpha.txt"]);
        stash_apply(fx.path(), &policy, 0, None).unwrap();
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
            &["Zeta.txt".to_owned()],
            None,
            Some(identity()),
            None,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(fx.path().join("Zeta.txt")).unwrap(),
            b"zeta\n",
            "the deletion is stashed, so the file is back"
        );
        assert!(stash_list(&fx).starts_with("WIP on main:"));

        stash_pop(fx.path(), &policy, 0, None).unwrap();
        assert!(!fx.path().join("Zeta.txt").exists());
    }
}
