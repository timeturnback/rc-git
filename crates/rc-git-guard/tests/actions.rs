//! The write path: branch and tag creation and deletion, gated by the policy, with backup
//! refs proving the safety net works.

use rc_git_guard::actions::{
    ActionError, Author, Identity, create_branch, create_tag, delete_branch, delete_tag,
    plan_branch_delete,
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

    use rc_git_core::{BlobDiff, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT, Oid, Repository};
    use rc_git_guard::Policy;
    use rc_git_guard::actions::{
        ActionError, Author, FileSelection, HunkStamp, Identity, commit, configured_identity,
        discard_files, discard_hunks,
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
            "modes",
            explicit(),
            Some(commit_id(&head)),
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
    fn a_path_under_a_submodule_reads_as_absent_from_head() {
        let fx = Fixture::standard();
        let gitlink = "160000,1111111111111111111111111111111111111111,sub";
        fx.git(&["update-index", "--add", "--cacheinfo", gitlink]);
        fx.git(&["commit", "-q", "-m", "add submodule"]);
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &Policy::default(),
            "under a gitlink",
            explicit(),
            Some(commit_id(&head)),
            &[whole("sub/x.c")],
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
            "into a submodule",
            explicit(),
            Some(commit_id(&head)),
            &[whole("alpha.txt"), whole("sub/alpha.txt")],
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
            "case",
            explicit(),
            Some(commit_id(&head)),
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
            explicit(),
            Some(commit_id(&head)),
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
            explicit(),
            Some(commit_id(&head)),
            &[
                whole("alpha.txt"),
                whole("brand-new.txt"),
                whole("Zeta.txt"),
            ],
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
    fn a_held_index_lock_refuses_the_commit_before_the_branch_moves() {
        let fx = Fixture::standard();
        let lock = fx.path().join(".git/index.lock");
        fx.write("alpha.txt", b"changed\n");
        fx.write(".git/index.lock", b"");
        let head = fx.rev_parse("HEAD");

        let err = commit(
            fx.path(),
            &Policy::default(),
            "locked",
            explicit(),
            Some(commit_id(&head)),
            &[whole("alpha.txt")],
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
            "retried",
            explicit(),
            Some(commit_id(&head)),
            &[whole("alpha.txt")],
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
            "Take the first hunk",
            explicit(),
            Some(commit_id(&head)),
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
            explicit(),
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
            explicit(),
            Some(commit_id(&fx.rev_parse("HEAD~1"))),
            &[whole("alpha.txt")],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::StaleRepository { .. }), "{err}");

        let err = commit(
            fx.path(),
            &policy,
            "msg",
            Author::default(),
            Some(commit_id(&head)),
            &[whole("alpha.txt")],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::MissingIdentity), "{err}");
        assert!(
            err.to_string()
                .contains("reads only this repository's .git/config (not ~/.gitconfig)"),
            "{err}"
        );

        let err = commit(
            fx.path(),
            &policy,
            "   ",
            explicit(),
            Some(commit_id(&head)),
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
            explicit(),
            Some(commit_id(&head)),
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
            Author::default(),
            Some(commit_id(&head)),
            &[whole("alpha.txt")],
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
            "Who wrote this",
            author,
            Some(commit_id(&head)),
            &[whole("alpha.txt")],
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
            "msg",
            explicit(),
            Some(commit_id(&head)),
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
            "first hunk",
            explicit(),
            Some(commit_id(&head)),
            &[FileSelection::Hunks {
                path: "alpha.txt".to_owned(),
                hunks: found,
            }],
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
            "msg",
            explicit(),
            Some(commit_id(&head)),
            &[FileSelection::Hunks {
                path: "alpha.txt".to_owned(),
                hunks: vec![],
            }],
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
                explicit(),
                Some(commit_id(&head)),
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
            explicit(),
            Some(commit_id(&head)),
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
    use rc_git_guard::actions::{
        ActionError, Author, Identity, stash_apply, stash_drop, stash_pop, stash_push,
    };
    use rc_git_testkit::Fixture;

    use super::{explicit, porcelain, refs};

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
            explicit(),
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
            &["alpha.txt".to_owned()],
            None,
            author,
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
            &["alpha.txt".to_owned(), "src".to_owned()],
            None,
            explicit(),
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

        stash_drop(fx.path(), &policy, 1, Some("drop")).unwrap();
        assert_eq!(stash_list(&fx), "On main: second");
        assert_eq!(
            fx.rev_parse("refs/rc-git/backup/stash/1"),
            older,
            "the backup keeps the stash git called stash@{{1}}"
        );
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
            explicit(),
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
            explicit(),
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
                explicit(),
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
            explicit(),
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
            explicit(),
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
    use rc_git_guard::actions::{ActionError, FileSelection, checkout_branch, commit, stash_push};
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
            "resolved",
            explicit(),
            Some(head_oid(fx)),
            &[FileSelection::Whole {
                path: "README.md".to_owned(),
            }],
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
            &["alpha.txt".to_owned()],
            None,
            explicit(),
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
