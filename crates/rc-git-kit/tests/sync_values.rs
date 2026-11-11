//! Ported from `SyncModelTests.swift`: the value suites (`SyncTitlesTests`,
//! `ReconcileCommandTests`, `SyncReportTests`) and the two pure tests of the model suite,
//! the credential sheet's check and which failure earns a prompt.

use rc_git_guard::actions::{ActionError, UpstreamStatus};
use rc_git_kit::guard_bridge::Credentials;
use rc_git_kit::platform::credentials::CredentialKind;
use rc_git_kit::sync::{
    CredentialEntry, CredentialRetry, SyncAvailability, SyncConditions, SyncStep, SyncSummary,
    back_to_default_title, credential_retry, idle_sync_tooltip, pull_title, push_title,
    reconcile_command,
};

fn upstream(ahead: usize, behind: usize, published: bool) -> UpstreamStatus {
    UpstreamStatus {
        branch: "main".to_owned(),
        remote: "origin".to_owned(),
        upstream_ref: "refs/remotes/origin/main".to_owned(),
        url: "https://example.com/repo.git".to_owned(),
        host: "example.com".to_owned(),
        ahead,
        behind,
        has_upstream: true,
        published,
        uses_ssh: false,
    }
}

fn no_upstream(branch: &str) -> UpstreamStatus {
    UpstreamStatus {
        branch: branch.to_owned(),
        remote: String::new(),
        upstream_ref: String::new(),
        url: String::new(),
        host: String::new(),
        ahead: 0,
        behind: 0,
        has_upstream: false,
        published: false,
        uses_ssh: false,
    }
}

#[test]
fn no_status_yet_shows_the_bare_verbs() {
    assert_eq!(pull_title(None), "Pull");
    assert_eq!(push_title(None), "Push");
}

#[test]
fn up_to_date_shows_the_bare_verbs() {
    let status = upstream(0, 0, true);
    assert_eq!(pull_title(Some(&status)), "Pull");
    assert_eq!(push_title(Some(&status)), "Push");
}

#[test]
fn counts_from_the_last_status_pluralize() {
    let status = upstream(3, 1, true);
    assert_eq!(push_title(Some(&status)), "Push 3 Commits");
    assert_eq!(pull_title(Some(&status)), "Pull 1 Commit");
}

fn idle(status: Option<&UpstreamStatus>) -> SyncConditions<'_> {
    SyncConditions {
        status,
        busy: false,
        cancellable: false,
        has_repository: true,
        has_working_tree: true,
    }
}

fn availability(conditions: SyncConditions<'_>) -> SyncAvailability {
    SyncAvailability::new(conditions)
}

/// Whether Fetch, Pull, Push and Sync are all enabled, or all disabled; `None` when the four
/// disagree, which no surface may ever see.
fn remote_controls(availability: SyncAvailability) -> Option<bool> {
    let all = [
        availability.can_fetch,
        availability.can_pull,
        availability.can_push,
        availability.can_sync,
    ];
    all.iter().all(|flag| *flag == all[0]).then_some(all[0])
}

#[test]
fn remote_controls_need_a_known_upstream_and_an_idle_model() {
    let upstream = upstream(0, 0, true);
    let none = no_upstream("main");
    assert_eq!(
        remote_controls(availability(idle(Some(&upstream)))),
        Some(true)
    );
    assert_eq!(
        remote_controls(availability(idle(None))),
        Some(false),
        "no status read yet"
    );
    assert_eq!(
        remote_controls(availability(idle(Some(&none)))),
        Some(false),
        "fetch, pull, push and sync all refuse a branch with no upstream"
    );
    let busy = SyncConditions {
        busy: true,
        ..idle(Some(&upstream))
    };
    assert_eq!(remote_controls(availability(busy)), Some(false));
    let empty = SyncConditions {
        has_repository: false,
        ..idle(Some(&upstream))
    };
    assert_eq!(
        remote_controls(availability(empty)),
        Some(false),
        "an empty window has nothing to sync"
    );
}

#[test]
fn back_to_default_needs_an_idle_checkout_but_no_upstream() {
    let none = no_upstream("topic");
    assert!(availability(idle(Some(&none))).can_back_to_default);
    assert!(
        availability(idle(None)).can_back_to_default,
        "the switch needs no status read"
    );
    let bare = SyncConditions {
        has_working_tree: false,
        ..idle(None)
    };
    assert!(!availability(bare).can_back_to_default, "a bare repository");
    let busy = SyncConditions {
        busy: true,
        ..idle(None)
    };
    assert!(!availability(busy).can_back_to_default);
    let empty = SyncConditions {
        has_repository: false,
        ..idle(None)
    };
    assert!(!availability(empty).can_back_to_default, "an empty window");
}

#[test]
fn the_sync_button_is_cancel_while_a_cancellable_step_runs() {
    let upstream = upstream(0, 0, true);
    let none = no_upstream("main");
    assert!(availability(idle(Some(&upstream))).sync_button_enabled());
    assert!(
        !availability(idle(Some(&none))).sync_button_enabled(),
        "agrees with the Sync menu item"
    );
    assert!(!availability(idle(None)).sync_button_enabled());
    let empty = SyncConditions {
        has_repository: false,
        ..idle(Some(&upstream))
    };
    assert!(!availability(empty).sync_button_enabled());
    let fetching = availability(SyncConditions {
        busy: true,
        cancellable: true,
        ..idle(Some(&upstream))
    });
    assert!(fetching.can_cancel);
    assert!(fetching.sync_button_enabled());
    assert!(!fetching.can_sync);
    let pulling = availability(SyncConditions {
        busy: true,
        ..idle(Some(&upstream))
    });
    assert!(!pulling.can_cancel);
    assert!(
        !pulling.sync_button_enabled(),
        "a pull or checkout runs to completion"
    );
    let nothing_running = availability(SyncConditions {
        cancellable: true,
        ..idle(Some(&upstream))
    });
    assert!(!nothing_running.can_cancel, "nothing is running");
}

#[test]
fn the_idle_tooltip_reads_the_last_status() {
    assert_eq!(idle_sync_tooltip(None, None), "Sync");
    assert_eq!(
        idle_sync_tooltip(Some(&upstream(0, 0, true)), None),
        "Sync — up to date"
    );
    assert_eq!(
        idle_sync_tooltip(Some(&upstream(2, 3, true)), None),
        "Sync — 2 to push, 3 to pull"
    );
    assert_eq!(
        idle_sync_tooltip(Some(&upstream(0, 0, false)), None),
        "Sync — not published",
        "a never-pushed branch is not up to date"
    );
    assert_eq!(
        idle_sync_tooltip(None, Some("HEAD is detached")),
        "Sync — couldn't read upstream status: HEAD is detached"
    );
}

#[test]
fn back_to_default_names_the_branch_once_it_is_known() {
    assert_eq!(back_to_default_title(None), "Back to the Default Branch");
    assert_eq!(back_to_default_title(Some("master")), "Back to master");
}

#[test]
fn a_plain_path_stays_bare() {
    assert_eq!(
        reconcile_command("/Users/me/WorkSpace/rc-git"),
        "cd /Users/me/WorkSpace/rc-git && git pull --rebase"
    );
}

#[test]
fn a_path_with_a_space_is_quoted() {
    assert_eq!(
        reconcile_command("/Users/me/My Repos/app"),
        "cd '/Users/me/My Repos/app' && git pull --rebase"
    );
}

fn summary(
    pulled_files: usize,
    cancelled_after: Option<SyncStep>,
    notice: Option<&str>,
) -> SyncSummary {
    SyncSummary {
        fetched_refs: 1,
        pulled_files,
        pushed_commits: 0,
        cancelled_after,
        notice: notice.map(str::to_owned),
    }
}

#[test]
fn a_workflow_that_ran_to_the_end_with_nothing_to_say_reports_nothing() {
    assert_eq!(summary(2, None, None).report(), None);
}

#[test]
fn a_cancel_after_a_finished_pull_is_explained_not_warned() {
    let report = summary(2, Some(SyncStep::Pull), None).report();
    let report = report.unwrap_or_else(|| unreachable!("a cancel is explained"));
    assert!(!report.is_problem);
    assert_eq!(report.title, "Sync cancelled");
    assert!(report.message.contains("pull had already finished"));
    assert!(report.message.contains("nothing was pushed"));
}

#[test]
fn a_cancel_after_a_finished_switch_says_the_switch_stands() {
    let report = summary(4, Some(SyncStep::Checkout), None).report();
    let report = report.unwrap_or_else(|| unreachable!("a cancel is explained"));
    assert!(!report.is_problem);
    assert!(
        report
            .message
            .contains("branch switch had already finished")
    );
}

#[test]
fn a_notice_is_a_problem() {
    let report = summary(0, None, Some("token not saved")).report();
    let report = report.unwrap_or_else(|| unreachable!("a notice is reported"));
    assert!(report.is_problem);
    assert_eq!(report.message, "token not saved");
}

#[test]
fn a_notice_after_a_cancel_carries_both() {
    let report = summary(1, Some(SyncStep::Checkout), Some("no reflog line")).report();
    let report = report.unwrap_or_else(|| unreachable!("a notice is reported"));
    assert!(report.is_problem);
    assert!(
        report
            .message
            .contains("branch switch had already finished")
    );
    assert!(report.message.ends_with("no reflog line"));
}

#[test]
fn adding_a_notice_joins_it_after_the_one_already_there() {
    let joined = summary(0, None, Some("first")).adding_notice(Some("second"));
    assert_eq!(joined.notice.as_deref(), Some("first\nsecond"));
    assert_eq!(summary(0, None, None).adding_notice(None).notice, None);
}

fn problem(entry: &CredentialEntry) -> Option<&'static str> {
    match entry {
        CredentialEntry::Invalid(missing) => Some(missing),
        CredentialEntry::Valid(_) => None,
    }
}

#[test]
fn the_credential_sheet_refuses_empty_fields() {
    let https = CredentialKind::HttpsToken;
    let ssh = CredentialKind::SshPassphrase;
    assert_eq!(
        problem(&CredentialEntry::check(https, " ", "t")),
        Some("Enter a username.")
    );
    assert_eq!(
        problem(&CredentialEntry::check(https, "u", "")),
        Some("Enter a token.")
    );
    assert_eq!(
        problem(&CredentialEntry::check(ssh, "", "")),
        Some("Enter the passphrase.")
    );
    assert_eq!(
        CredentialEntry::check(https, " u ", "t"),
        CredentialEntry::Valid(Credentials {
            username: "u".to_owned(),
            token: "t".to_owned()
        })
    );
    assert_eq!(
        CredentialEntry::check(ssh, "", "p"),
        CredentialEntry::Valid(Credentials {
            username: String::new(),
            token: "p".to_owned()
        }),
        "a passphrase alone is valid over ssh"
    );
}

#[test]
fn only_the_error_the_secret_can_fix_earns_a_prompt() {
    let locked = ActionError::PassphraseRequired {
        op: "fetch",
        host: "h".to_owned(),
        key: "/k/id".to_owned(),
    };
    let rejected = ActionError::AuthRequired {
        op: "fetch",
        host: "h".to_owned(),
    };
    let refused = ActionError::CredentialHostRefused {
        op: "fetch",
        host: "h".to_owned(),
        requested: "other".to_owned(),
    };
    let other = ActionError::Denied {
        reason: "m".to_owned(),
    };
    let ssh = CredentialKind::SshPassphrase;
    let https = CredentialKind::HttpsToken;
    assert_eq!(
        credential_retry(&locked, ssh),
        CredentialRetry::Prompt {
            key: Some("/k/id".to_owned())
        }
    );
    assert_eq!(credential_retry(&rejected, ssh), CredentialRetry::None);
    assert_eq!(
        credential_retry(&rejected, https),
        CredentialRetry::Prompt { key: None }
    );
    assert_eq!(credential_retry(&locked, https), CredentialRetry::None);
    assert_eq!(credential_retry(&other, ssh), CredentialRetry::None);
    assert_eq!(credential_retry(&refused, https), CredentialRetry::None);
    assert_eq!(credential_retry(&refused, ssh), CredentialRetry::None);
}
