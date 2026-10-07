//! The write path: branch and tag creation and deletion, gated by the policy, with backup
//! refs proving the safety net works.

use rc_git_guard::actions::{
    ActionError, Identity, create_branch, create_tag, delete_branch, delete_tag, plan_branch_delete,
};
use rc_git_guard::{Decision, Policy, Risk};
use rc_git_testkit::Fixture;

fn refs(fx: &Fixture) -> String {
    fx.git_output(&["for-each-ref", "--format=%(refname)"])
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
    assert!(delete_tag(fx.path(), &policy, "V1.0", Some("V1.0")).is_err());
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

    use rc_git_core::{BlobDiff, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT, Repository};
    use rc_git_guard::Policy;
    use rc_git_guard::actions::{
        ActionError, FileSelection, HunkStamp, commit, discard_files, discard_hunks,
    };
    use rc_git_testkit::Fixture;

    use super::{identity, porcelain};

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
            "modes",
            Some(identity()),
            Some(&head),
            &selections,
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
            "case",
            Some(identity()),
            Some(&head),
            &[whole("ALPHA.TXT")],
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
            "case",
            Some(identity()),
            Some(&head),
            &[whole("docs/guide.md")],
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
        assert!(
            matches!(err, ActionError::DetachedHead { op: "commit" }),
            "{err}"
        );
        assert_eq!(fx.rev_parse("main"), head, "nothing moved");
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
                "bad path",
                Some(identity()),
                Some(&head),
                &[whole(path)],
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
            "outside",
            Some(identity()),
            Some(&head),
            &[whole("up/secret.txt")],
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
    use rc_git_guard::actions::{ActionError, stash_apply, stash_drop, stash_pop, stash_push};
    use rc_git_testkit::Fixture;

    use super::{identity, porcelain, refs};

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
    fn push_refuses_a_head_directory_before_writing_anything() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"stash me\n");
        fx.remove("src");

        let err = stash_push(
            fx.path(),
            &policy,
            &["alpha.txt".to_owned(), "src".to_owned()],
            None,
            Some(identity()),
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
        assert!(!refs(&fx).contains("refs/stash"));
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

        let err = stash_apply(fx.path(), &policy, 0, None).unwrap_err();
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
    fn apply_refuses_a_dirty_path_and_touches_nothing() {
        let fx = Fixture::standard();
        let policy = Policy::default();
        fx.write("alpha.txt", b"stash me\n");
        fx.git(&["stash", "push", "-q", "-m", "kept"]);
        fx.write("alpha.txt", b"conflicting edit\n");

        let err = stash_apply(fx.path(), &policy, 0, None).unwrap_err();
        assert!(
            matches!(&err, ActionError::DirtyPath { op: "stash apply", path } if path == "alpha.txt"),
            "{err}"
        );
        assert!(
            err.to_string().starts_with("stash apply: 'alpha.txt'"),
            "{err}"
        );
        let err = stash_pop(fx.path(), &policy, 0, None).unwrap_err();
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

    #[test]
    fn stash_push_on_a_detached_head_names_stash_push() {
        let fx = Fixture::standard();
        fx.git(&["checkout", "-q", "--detach", "HEAD"]);
        fx.write("alpha.txt", b"changed\n");
        let err = stash_push(
            fx.path(),
            &Policy::default(),
            &["alpha.txt".to_owned()],
            None,
            Some(identity()),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, ActionError::DetachedHead { op: "stash push" }),
            "{err}"
        );
        assert_eq!(stash_list(&fx), "");
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
                &[(*path).to_owned()],
                None,
                Some(identity()),
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
            &["docs/guide.md".to_owned()],
            None,
            Some(identity()),
            None,
        )
        .unwrap();
        let stashed = stash_list(&fx);
        let outside = link_docs_outside(&fx);

        let err = stash_push(
            fx.path(),
            &policy,
            &["docs/guide.md".to_owned()],
            None,
            Some(identity()),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains(CROSSES_LINK), "push: {err}");
        let err = stash_pop(fx.path(), &policy, 0, None).unwrap_err();
        assert!(err.to_string().contains(CROSSES_LINK), "pop: {err}");

        assert_eq!(
            std::fs::read(outside.path().join("guide.md")).unwrap(),
            b"outside guide\n"
        );
        assert_eq!(stash_list(&fx), stashed, "the stash survives");
    }
}

mod checkout {
    use rc_git_guard::Policy;
    use rc_git_guard::actions::{ActionError, checkout_branch, create_branch};
    use rc_git_testkit::Fixture;

    use super::commit_and_discard::CROSSES_LINK;
    use super::porcelain;

    fn head_branch(fx: &Fixture) -> String {
        fx.git_output(&["rev-parse", "--abbrev-ref", "HEAD"])
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

        checkout_branch(fx.path(), &Policy::default(), "feature").unwrap();
        assert_eq!(head_branch(&fx), "feature");
        assert_eq!(fx.rev_parse("HEAD"), fx.rev_parse("feature"));
        assert_eq!(porcelain(&fx), "");
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
