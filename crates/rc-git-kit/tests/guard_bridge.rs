//! Ported from the value tests in `RepositoryGuardTests.swift`, plus the status mapping the
//! FFI's `action_status`/`finish_remote` tests pinned.
#![expect(clippy::unwrap_used, reason = "the oid helper builds a fixed valid id")]

use std::collections::HashMap;

use rc_git_core::Oid;
use rc_git_guard::actions::{
    ActionError, CheckoutOutcome, Committed, RemoteAuth, fetch_upstream, plan_branch_delete,
    plan_commit, plan_discard, plan_stash_drop, plan_stash_pop, plan_tag_delete,
};
use rc_git_guard::{Policy, Risk};
use rc_git_highlight::{Language, highlight};
use rc_git_kit::guard_bridge::{
    CancelToken, Credentials, DecisionKind, FailureKind, GuardDecision, GuardFailure,
    READ_FAILURE_TITLE, checkout_warning, classify, commit_warning, entered_ref_name,
    failure_response, failure_title,
};
use rc_git_testkit::Fixture;

fn oid() -> Oid {
    Oid::from_hex(&"a".repeat(40)).unwrap()
}

#[test]
fn a_commit_with_housekeeping_left_undone_warns_for_each_part() {
    let commit = |index_refreshed, reflog_written| Committed {
        oid: oid(),
        branch: "main".to_owned(),
        index_refreshed,
        reflog_written,
    };
    assert_eq!(commit_warning(&commit(true, true)), None);
    let stale = commit_warning(&commit(false, true)).unwrap();
    assert!(stale.contains("git reset -q -- <paths>"), "{stale}");
    assert!(!stale.contains("HEAD@{1}"), "{stale}");
    let unlogged = commit_warning(&commit(true, false)).unwrap();
    assert!(unlogged.contains("HEAD@{1}"), "{unlogged}");
    assert!(!unlogged.contains("git reset"), "{unlogged}");
    let both = commit_warning(&commit(false, false)).unwrap();
    assert!(both.contains("git reset -q -- <paths>") && both.contains("HEAD@{1}"));
    assert_eq!(both.lines().count(), 2, "one line per part");
}

#[test]
fn a_switch_without_its_reflog_line_warns() {
    let outcome = |reflog_written| CheckoutOutcome {
        branch: "topic".to_owned(),
        new_head: oid(),
        files_changed: 1,
        already_on_branch: false,
        reflog_written,
    };
    assert_eq!(checkout_warning(&outcome(true)), None);
    assert_eq!(
        checkout_warning(&outcome(false)).as_deref(),
        Some(
            "Switched to 'topic', but .git/logs/HEAD did not record it, so `git checkout -` and \
             HEAD@{1} skip the switch."
        )
    );
}

#[test]
fn decisions_flatten_with_their_challenge() {
    let policy = Policy::default();
    let allow = GuardDecision::new(&plan_commit(&policy, 2), false);
    assert_eq!(allow.kind, DecisionKind::Allow);
    assert_eq!(allow.risk, Risk::Safe);
    assert_eq!(allow.summary, "");
    assert_eq!(allow.typed_phrase, None);

    let drop = GuardDecision::new(&plan_stash_drop(&policy, 1, "On main: wip"), false);
    assert_eq!(drop.kind, DecisionKind::Confirm);
    assert_eq!(drop.typed_phrase.as_deref(), Some("drop"));
    assert!(drop.backup_ref);
    assert!(
        drop.summary.contains("stash@{1} \"On main: wip\""),
        "{}",
        drop.summary
    );

    let pop = GuardDecision::new(&plan_stash_pop(&policy, 0, "On main: wip"), false);
    assert!(pop.backup_ref);
    assert_eq!(pop.typed_phrase, None);

    let discard = GuardDecision::new(&plan_discard(&policy, 4), false);
    assert_eq!(discard.kind, DecisionKind::Confirm);
    assert_eq!(discard.typed_phrase, None);
    assert_eq!(discard.risk, Risk::Destructive);

    let tag = GuardDecision::new(&plan_tag_delete(&policy, "v1.0"), false);
    assert_eq!(tag.typed_phrase.as_deref(), Some("v1.0"));
    assert_eq!(tag.reason, None);
}

#[test]
fn a_denied_branch_delete_carries_its_reason_and_merged_flag() {
    let fixture = Fixture::standard();
    let plan = plan_branch_delete(fixture.path(), &Policy::default(), "main").unwrap();
    let decision = GuardDecision::new(&plan.decision, plan.merged);
    assert_eq!(decision.kind, DecisionKind::Deny);
    assert_eq!(decision.risk, Risk::Destructive);
    assert!(
        decision
            .reason
            .as_deref()
            .is_some_and(|reason| !reason.is_empty())
    );
    assert_eq!(decision.merged, plan.merged);
    assert!(!decision.backup_ref);
}

#[test]
fn a_stale_repository_is_a_notice_and_every_other_failure_an_alert() {
    let stale = ActionError::StaleRepository { op: "stash drop" };
    assert_eq!(
        failure_response(&stale),
        GuardFailure::Stale {
            notice: "stash drop: the repository changed since the list was built; refresh and \
                     retry\nThe list has been reloaded; check it and try again."
                .to_owned()
        }
    );
    let dirty = ActionError::DirtyPath {
        op: "discard",
        path: "a.txt".to_owned(),
    };
    assert_eq!(
        failure_response(&dirty),
        GuardFailure::Failed {
            message: dirty.to_string()
        }
    );
    let identity = ActionError::MissingIdentity { op: "commit" };
    assert_eq!(
        failure_response(&identity),
        GuardFailure::Failed {
            message: identity.to_string()
        }
    );
    let diverged = ActionError::Diverged {
        branch: "main".to_owned(),
        ahead: 1,
        behind: 2,
    };
    assert_eq!(
        failure_response(&diverged),
        GuardFailure::Failed {
            message:
                "cannot fast-forward 'main': local is 1 ahead and 2 behind; reconcile with git"
                    .to_owned()
        }
    );
}

#[test]
fn each_error_the_window_branches_on_has_its_own_kind() {
    let cases = [
        (
            ActionError::MissingIdentity { op: "commit" },
            FailureKind::IdentityRequired,
        ),
        (
            ActionError::StaleRepository { op: "pull" },
            FailureKind::Stale,
        ),
        (
            ActionError::AuthRequired {
                op: "fetch",
                host: "example.com".to_owned(),
            },
            FailureKind::AuthRequired,
        ),
        (
            ActionError::CredentialHostRefused {
                op: "fetch",
                host: "a.example".to_owned(),
                requested: "b.example".to_owned(),
            },
            FailureKind::CredentialHostRefused,
        ),
        (
            ActionError::PassphraseRequired {
                op: "fetch",
                host: "example.com".to_owned(),
                key: "/home/me/.ssh/id_ed25519".to_owned(),
            },
            FailureKind::PassphraseRequired {
                key: "/home/me/.ssh/id_ed25519".to_owned(),
            },
        ),
        (
            ActionError::Diverged {
                branch: "main".to_owned(),
                ahead: 1,
                behind: 1,
            },
            FailureKind::Diverged,
        ),
        (
            ActionError::PushRejected {
                branch: "main".to_owned(),
            },
            FailureKind::Diverged,
        ),
        (
            ActionError::UnknownHostKey {
                op: "fetch",
                host: "example.com".to_owned(),
                key_type: "ssh-ed25519".to_owned(),
                fingerprint: "SHA256:abc".to_owned(),
            },
            FailureKind::HostKeyUnknown {
                host_key: "ssh-ed25519 SHA256:abc".to_owned(),
            },
        ),
        (
            ActionError::Cancelled { op: "push" },
            FailureKind::Cancelled,
        ),
    ];
    for (error, kind) in cases {
        assert_eq!(classify(&error), kind, "{error}");
    }
}

/// A refused agent signature, a mismatched or revoked host key, and every local refusal are
/// plain alerts: nothing the window can prompt for fixes them.
#[test]
fn errors_no_prompt_can_fix_are_plain_failures() {
    let cases = [
        ActionError::AgentSignRefused {
            op: "fetch",
            host: "example.com".to_owned(),
            key: "me SHA256:abc".to_owned(),
            agent: "/tmp/agent.sock".to_owned(),
        },
        ActionError::HostKeyMismatch {
            op: "fetch",
            host: "example.com".to_owned(),
            key_type: "ssh-ed25519".to_owned(),
            fingerprint: "SHA256:abc".to_owned(),
            approved: None,
        },
        ActionError::HostKeyRevoked {
            op: "fetch",
            host: "example.com".to_owned(),
            key_type: "ssh-ed25519".to_owned(),
            fingerprint: "SHA256:abc".to_owned(),
        },
        ActionError::Denied {
            reason: "protected".to_owned(),
        },
        ActionError::NotConfirmed {
            op: "delete branch",
            name: "topic".to_owned(),
            phrase: "topic".to_owned(),
            given: false,
        },
        ActionError::NoUpstream {
            op: "push",
            branch: "topic".to_owned(),
        },
        ActionError::DetachedHead { op: "commit" },
    ];
    for error in cases {
        assert_eq!(classify(&error), FailureKind::Failed, "{error}");
    }
}

#[test]
fn names_the_verb_in_lower_case() {
    assert_eq!(
        failure_title("Delete Branch"),
        "rc-git could not delete branch"
    );
    assert_eq!(failure_title("Commit"), "rc-git could not commit");
}

#[test]
fn never_reads_as_a_failed_read() {
    for verb in [
        "Create Branch",
        "Delete Tag",
        "Stash",
        "Discard Hunk",
        "Commit",
    ] {
        assert_ne!(failure_title(verb), READ_FAILURE_TITLE);
    }
}

#[test]
fn a_ref_name_sheet_needs_a_name_left_after_trimming() {
    assert_eq!(entered_ref_name(""), None);
    assert_eq!(entered_ref_name("  \t\n"), None);
    assert_eq!(entered_ref_name(" topic\n"), Some("topic"));
    assert_eq!(entered_ref_name("v1.0"), Some("v1.0"));
}

/// A token must not leak through any way Rust turns a value into text.
#[test]
fn every_text_form_omits_the_token() {
    let credentials = Credentials {
        username: "octocat".to_owned(),
        token: "TOKEN-VALUE".to_owned(),
    };
    let renderings = [
        format!("{credentials:?}"),
        format!("{credentials:#?}"),
        format!("{:?}", Some(&credentials)),
        format!("{:?}", [&credentials]),
        format!("{:?}", HashMap::from([("host", &credentials)])),
    ];
    for rendering in renderings {
        assert!(!rendering.contains("TOKEN-VALUE"), "{rendering}");
        assert!(rendering.contains("<redacted>"), "{rendering}");
        assert!(rendering.contains("octocat"), "{rendering}");
    }
    let held = format!("{:?}", Err::<(), _>(credentials));
    assert!(!held.contains("TOKEN-VALUE"), "{held}");
}

#[test]
fn a_fresh_token_is_lowered_and_a_signal_raises_both_flags() {
    let token = CancelToken::new();
    assert!(!token.is_signalled());
    assert!(!token.remote().is_cancelled());
    assert_eq!(
        token.highlight().load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert!(highlight(b"fn main() {}\n", Language::Rust, Some(token.highlight())).is_ok());

    std::thread::scope(|scope| {
        scope.spawn(|| token.signal());
    });
    assert!(token.is_signalled());
    assert!(token.remote().is_cancelled());
    let highlighted = highlight(b"fn main() {}\n", Language::Rust, Some(token.highlight()));
    assert_eq!(
        highlighted.map_err(|err| err.to_string()),
        Err("highlighting rust: Cancelled".to_owned())
    );
}

/// The guard polls the token before it opens the repository, so a signalled fetch stops
/// without touching the network and classifies as a cancel, not a failure.
#[test]
fn a_signalled_token_stops_a_fetch_before_it_reaches_the_network() {
    let fixture = Fixture::standard();
    let token = CancelToken::new();
    token.signal();
    let err = fetch_upstream(
        fixture.path(),
        &Policy::default(),
        RemoteAuth::default(),
        token.remote(),
    )
    .unwrap_err();
    assert_eq!(classify(&err), FailureKind::Cancelled, "{err}");
}
