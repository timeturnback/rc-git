//! Ported from the `SyncModelTests` suite of `SyncModelTests.swift`: the workflows over a
//! scripted `RepositoryActions`, the credential and host-key loop, cancel, and the coalesced
//! status read. The value suites (titles, availability, reconcile command, reports) are in
//! `sync_values.rs`.
//!
//! Swift's performer let a test run code on the step's own thread; here a step is held with
//! `Stub::hold`, the test awaits its arrival, acts on the model, then opens the gate. A
//! cancel is signalled from inside the running step either way.

#[path = "support/sync.rs"]
mod support;

use std::rc::Rc;

use gpui::{Entity, TestAppContext};
use rc_git_core::DefaultBranch;
use rc_git_guard::actions::ActionError;
use rc_git_kit::platform::credentials::CredentialKind;
use rc_git_kit::platform::host_keys::ApprovedHostsModel;
use rc_git_kit::sync::{
    CredentialReply, HostKeyReply, SyncEvent, SyncModel, SyncState, SyncStep, SyncSummary,
    checkout_warning, pull_title, push_title,
};
use support::{
    Script, SharedHostKeys, Step, Stub, TestStore, answer_credentials, answer_host_keys,
    auth_required, credentials, failure, fetched, model, no_upstream, passphrase_required, pulled,
    pushed, refresh, ssh_upstream, state, states, switched, unknown_host_key, upstream,
};

const HTTPS: CredentialKind = CredentialKind::HttpsToken;
const SSH: CredentialKind = CredentialKind::SshPassphrase;

fn fixture(cx: &mut TestAppContext, script: Script) -> (Entity<SyncModel>, Stub) {
    let stub = Stub::new(script);
    let model = model(cx, &stub, &TestStore::default(), &SharedHostKeys::default());
    (model, stub)
}

fn with_store(
    cx: &mut TestAppContext,
    script: Script,
    store: &TestStore,
) -> (Entity<SyncModel>, Stub) {
    let stub = Stub::new(script);
    let model = model(cx, &stub, store, &SharedHostKeys::default());
    (model, stub)
}

fn with_host_keys(
    cx: &mut TestAppContext,
    script: Script,
    host_keys: &SharedHostKeys,
) -> (Entity<SyncModel>, Stub) {
    let stub = Stub::new(script);
    let model = model(cx, &stub, &TestStore::default(), host_keys);
    (model, stub)
}

fn summary(fetched_refs: usize, pulled_files: usize, pushed_commits: usize) -> SyncSummary {
    SyncSummary {
        fetched_refs,
        pulled_files,
        pushed_commits,
        cancelled_after: None,
        notice: None,
    }
}

fn done(fetched_refs: usize, pulled_files: usize, pushed_commits: usize) -> SyncState {
    SyncState::Done(summary(fetched_refs, pulled_files, pushed_commits))
}

fn full_sync(preflight: Option<rc_git_guard::actions::UpstreamStatus>) -> Script {
    Script::new(vec![preflight])
        .fetches(vec![Ok(fetched(1))])
        .pulls(vec![Ok(pulled(2))])
        .pushes(vec![Ok(pushed(1))])
}

async fn run(
    cx: &mut TestAppContext,
    model: &Entity<SyncModel>,
    workflow: fn(&mut SyncModel, &mut gpui::Context<SyncModel>) -> gpui::Task<()>,
) {
    model.update(cx, workflow).await;
}

fn failed_message(state: &SyncState) -> (SyncStep, String) {
    match state {
        SyncState::Failed { step, message } => (*step, message.clone()),
        other => (SyncStep::Checkout, format!("not failed: {other:?}")),
    }
}

fn auth_message(host: &str) -> String {
    auth_required(host).to_string()
}

#[gpui::test]
async fn a_full_sync_runs_all_three_steps_and_summarizes(cx: &mut TestAppContext) {
    let (model, stub) = fixture(cx, full_sync(Some(upstream("example.com", 0, 0))));
    let seen = states(cx, &model);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;
    assert_eq!(state(cx, &model), done(1, 2, 1));
    for step in [SyncStep::Fetch, SyncStep::Pull, SyncStep::Push] {
        assert!(
            seen.borrow().contains(&SyncState::Running(step)),
            "{step:?}"
        );
    }
    assert_eq!(stub.state().push_calls, 1);
}

#[gpui::test]
async fn auth_retry_prompts_stores_only_on_success(cx: &mut TestAppContext) {
    let store = TestStore::default();
    let script = Script::new(vec![Some(upstream("git.example.com", 0, 0))])
        .fetches(vec![Err(auth_required("git.example.com")), Ok(fetched(1))]);
    let (model, stub) = with_store(cx, script, &store);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("user", "sekret")),
    );
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;

    assert_eq!(asked.borrow()[0].host, "git.example.com");
    assert!(matches!(state(cx, &model), SyncState::Done(_)));
    assert_eq!(
        stub.fetch_tokens(),
        [None, Some("sekret".to_owned())],
        "the first attempt is anonymous"
    );
    assert_eq!(
        stub.state().fetch_requests[1]
            .credentials
            .as_ref()
            .map(|c| c.username.clone()),
        Some("user".to_owned())
    );
    assert_eq!(
        store.stored("git.example.com", HTTPS).as_deref(),
        Some("sekret")
    );
}

#[gpui::test]
async fn a_sync_reads_the_store_once(cx: &mut TestAppContext) {
    let store = TestStore::with(&[("example.com", HTTPS, credentials("user", "saved"))]);
    let (model, stub) = with_store(cx, full_sync(Some(upstream("example.com", 0, 0))), &store);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;
    assert_eq!(store.queried_hosts(), ["example.com"]);
    assert_eq!(stub.fetch_tokens(), [Some("saved".to_owned())]);
    assert_eq!(stub.push_tokens(), [Some("saved".to_owned())]);
}

#[gpui::test]
async fn stored_credentials_are_tried_first_without_a_prompt(cx: &mut TestAppContext) {
    let store = TestStore::with(&[("git.example.com", HTTPS, credentials("u", "t"))]);
    let script =
        Script::new(vec![Some(upstream("git.example.com", 0, 0))]).fetches(vec![Ok(fetched(1))]);
    let (model, stub) = with_store(cx, script, &store);
    let asked = answer_credentials(cx, &model, CredentialReply::Cancelled);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(state(cx, &model), done(1, 0, 0));
    assert_eq!(stub.fetch_tokens(), [Some("t".to_owned())]);
    assert!(asked.borrow().is_empty());
    assert_eq!(
        store.write_count(),
        0,
        "credentials nobody typed are never written back"
    );
}

#[gpui::test]
async fn a_stored_ssh_passphrase_is_tried_first_and_never_the_https_token(cx: &mut TestAppContext) {
    let store = TestStore::with(&[
        ("git.example.com", HTTPS, credentials("u", "https-token")),
        ("git.example.com", SSH, credentials("", "passphrase")),
    ]);
    let script =
        Script::new(vec![Some(ssh_upstream("git.example.com"))]).fetches(vec![Ok(fetched(1))]);
    let (model, stub) = with_store(cx, script, &store);
    let asked = answer_credentials(cx, &model, CredentialReply::Cancelled);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(state(cx, &model), done(1, 0, 0));
    assert_eq!(stub.fetch_tokens(), [Some("passphrase".to_owned())]);
    assert!(asked.borrow().is_empty());
    assert_eq!(store.write_count(), 0);
}

#[gpui::test]
async fn a_prompted_token_carries_from_the_fetch_to_the_push_of_one_sync(cx: &mut TestAppContext) {
    let store = TestStore::default();
    let mut script = full_sync(Some(upstream("example.com", 0, 0)));
    script.fetches.push_front(Err(auth_required("example.com")));
    let (model, stub) = with_store(cx, script, &store);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("user", "typed")),
    );
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;
    assert_eq!(asked.borrow().len(), 1);
    assert_eq!(store.queried_hosts().len(), 1);
    assert_eq!(stub.push_tokens(), [Some("typed".to_owned())]);
}

#[gpui::test]
async fn a_failed_store_read_still_prompts_and_the_failure_is_reported(cx: &mut TestAppContext) {
    let store = TestStore::failing_reads();
    let mut script = full_sync(Some(upstream("example.com", 0, 0)));
    script.fetches.push_front(Err(auth_required("example.com")));
    let (model, stub) = with_store(cx, script, &store);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("user", "typed")),
    );
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;

    assert_eq!(asked.borrow().len(), 1);
    assert_eq!(stub.fetch_tokens()[0], None, "falls back to anonymous");
    assert_eq!(stub.push_tokens(), [Some("typed".to_owned())]);
    let SyncState::Done(summary) = state(cx, &model) else {
        panic!("expected done");
    };
    assert_eq!(summary.pushed_commits, 1);
    let notice = summary.notice.unwrap_or_default();
    assert!(notice.contains("could not be read"), "{notice}");
    assert!(notice.contains("status -25293"), "{notice}");
    assert!(!notice.contains("typed"), "never the token");
}

#[gpui::test]
async fn a_failed_store_read_is_named_in_the_failure(cx: &mut TestAppContext) {
    let store = TestStore::failing_reads();
    let script = Script::upstream().fetches(vec![Err(auth_required("example.com"))]);
    let (model, _) = with_store(cx, script, &store);
    answer_credentials(cx, &model, CredentialReply::Cancelled);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    let (_, message) = failed_message(&state(cx, &model));
    assert!(message.contains("authentication canceled"), "{message}");
    assert!(
        message.contains("keychain read failed for 'example.com'"),
        "{message}"
    );
}

#[gpui::test]
async fn a_failed_store_write_keeps_the_step_and_says_the_token_was_not_saved(
    cx: &mut TestAppContext,
) {
    let store = TestStore::failing_writes();
    let mut script = full_sync(Some(upstream("example.com", 0, 0)));
    script.fetches.push_front(Err(auth_required("example.com")));
    let (model, stub) = with_store(cx, script, &store);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("user", "typed")),
    );
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;

    assert_eq!(asked.borrow().len(), 1);
    assert_eq!(stub.push_tokens(), [Some("typed".to_owned())]);
    assert_eq!(store.len(), 0);
    let SyncState::Done(summary) = state(cx, &model) else {
        panic!("expected done");
    };
    assert_eq!((summary.fetched_refs, summary.pushed_commits), (1, 1));
    assert_eq!(
        summary.notice.as_deref(),
        Some(
            "signed in, but the token was not saved: keychain write failed for 'example.com' \
             (status -25293)"
        )
    );
}

#[gpui::test]
async fn each_workflow_reads_the_store_afresh(cx: &mut TestAppContext) {
    let store = TestStore::default();
    let script = Script::upstream().fetches(vec![Ok(fetched(1)), Ok(fetched(1))]);
    let (model, _) = with_store(cx, script, &store);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    model.update(cx, SyncModel::reset);
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(store.queried_hosts().len(), 2);
}

#[gpui::test]
async fn fetch_with_no_prior_refresh_keys_credentials_on_the_real_host(cx: &mut TestAppContext) {
    let store = TestStore::with(&[("git.example.com", HTTPS, credentials("user", "saved"))]);
    let script =
        Script::new(vec![Some(upstream("git.example.com", 0, 0))]).fetches(vec![Ok(fetched(1))]);
    let (model, stub) = with_store(cx, script, &store);
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(state(cx, &model), done(1, 0, 0));
    assert_eq!(store.queried_hosts(), ["git.example.com"]);
    assert_eq!(
        stub.state().fetch_requests[0].host.as_deref(),
        Some("git.example.com")
    );
    assert_eq!(stub.fetch_tokens(), [Some("saved".to_owned())]);
}

#[gpui::test]
async fn an_empty_host_never_reads_stores_or_prompts_for_secrets(cx: &mut TestAppContext) {
    let store = TestStore::with(&[("", HTTPS, credentials("user", "kept-under-empty-host"))]);
    let host_keys = SharedHostKeys::with(&[("", "SHA256:poisoned")]);
    let stub = Stub::new(
        Script::new(vec![Some(upstream("", 0, 0))])
            .fetches(vec![Err(auth_required("")), Ok(fetched(1))]),
    );
    let model = model(cx, &stub, &store, &host_keys);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("user", "typed")),
    );
    let trusted = answer_host_keys(cx, &model, HostKeyReply::Trusted);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;

    assert_eq!(
        state(cx, &model),
        SyncState::Failed {
            step: SyncStep::Fetch,
            message: "cannot tell which host the upstream is on".to_owned()
        }
    );
    assert!(asked.borrow().is_empty() && trusted.borrow().is_empty());
    assert!(store.queried_hosts().is_empty());
    assert_eq!(store.len(), 1, "nothing new is stored");
    assert_eq!(
        stub.fetch_tokens(),
        [None],
        "one attempt, the poisoned item never sent"
    );
    assert_eq!(stub.fetch_host_keys(), [None]);
    assert_eq!(stub.state().fetch_requests[0].host, None);
    assert_eq!(
        host_keys.stored(),
        [(String::new(), "SHA256:poisoned".to_owned())]
    );
}

#[gpui::test]
async fn a_local_upstream_still_pulls_without_touching_the_store(cx: &mut TestAppContext) {
    let store = TestStore::default();
    let mut local = upstream("", 0, 0);
    local.remote = ".".to_owned();
    let script = Script::new(vec![Some(local)])
        .fetches(vec![Ok(fetched(0))])
        .pulls(vec![Ok(pulled(2))]);
    let (model, stub) = with_store(cx, script, &store);
    run(cx, &model, SyncModel::pull).await;
    assert_eq!(state(cx, &model), done(0, 2, 0));
    assert_eq!(stub.fetch_tokens(), [None]);
    assert_eq!(stub.state().pull_calls, 1);
    assert!(store.queried_hosts().is_empty());
}

#[gpui::test]
async fn rejected_credentials_are_never_stored_and_prompts_are_capped(cx: &mut TestAppContext) {
    let store = TestStore::default();
    let script = Script::upstream().fetches(vec![
        Err(auth_required("example.com")),
        Err(auth_required("example.com")),
        Err(auth_required("example.com")),
    ]);
    let (model, _) = with_store(cx, script, &store);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("user", "wrong")),
    );
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(
        state(cx, &model),
        SyncState::Failed {
            step: SyncStep::Fetch,
            message: format!("{} (gave up after 3 attempts)", auth_message("example.com"))
        },
        "the guard's reason is kept"
    );
    assert_eq!(
        asked.borrow().len(),
        2,
        "the attempt limit caps the prompt loop"
    );
    assert_eq!(store.len(), 0, "rejected credentials must never be stored");
}

#[gpui::test]
async fn cancelling_the_prompt_fails_the_step(cx: &mut TestAppContext) {
    let store = TestStore::default();
    let script = Script::upstream().fetches(vec![Err(auth_required("example.com"))]);
    let (model, _) = with_store(cx, script, &store);
    answer_credentials(cx, &model, CredentialReply::Cancelled);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    let (_, message) = failed_message(&state(cx, &model));
    assert!(message.contains("canceled"), "{message}");
    assert_eq!(store.len(), 0);
}

#[gpui::test]
async fn a_locked_key_that_never_unlocks_names_the_key_not_the_host(cx: &mut TestAppContext) {
    let script = Script::new(vec![Some(ssh_upstream("git.example.com"))]).fetches(vec![
        Err(passphrase_required("/k/id")),
        Err(passphrase_required("/k/id")),
        Err(passphrase_required("/k/id")),
    ]);
    let (model, _) = fixture(cx, script);
    answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("", "wrong")),
    );
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    let (_, message) = failed_message(&state(cx, &model));
    assert!(message.contains("the key '/k/id'"), "{message}");
    assert!(message.contains("gave up after 3 attempts"), "{message}");
    assert!(!message.contains("rejecting"), "{message}");
}

#[gpui::test]
async fn no_prompter_says_none_are_stored_rather_than_canceled(cx: &mut TestAppContext) {
    let script = Script::upstream().fetches(vec![Err(auth_required("example.com"))]);
    let (model, _) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    let (_, message) = failed_message(&state(cx, &model));
    assert_eq!(
        message,
        format!(
            "credentials required for 'example.com' and none are stored: {}",
            auth_message("example.com")
        )
    );
}

#[gpui::test]
async fn an_unavailable_prompt_after_stored_credentials_keeps_the_reason(cx: &mut TestAppContext) {
    let store = TestStore::with(&[("example.com", HTTPS, credentials("user", "stale"))]);
    let script = Script::upstream().fetches(vec![Err(auth_required("example.com"))]);
    let (model, _) = with_store(cx, script, &store);
    let asked = answer_credentials(cx, &model, CredentialReply::Unavailable);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(asked.borrow().len(), 1);
    let (_, message) = failed_message(&state(cx, &model));
    assert_eq!(
        message,
        format!(
            "{}; no prompt is available to ask again",
            auth_message("example.com")
        )
    );
}

#[gpui::test]
async fn a_dropped_prompt_answer_counts_as_unavailable(cx: &mut TestAppContext) {
    let script = Script::upstream().fetches(vec![Err(auth_required("example.com"))]);
    let (model, _) = fixture(cx, script);
    model.update(cx, |m, _| {
        m.set_credential_prompter(Rc::new(|_, answer, _| drop(answer)));
    });
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    let (_, message) = failed_message(&state(cx, &model));
    assert!(
        message.starts_with("credentials required for 'example.com'"),
        "{message}"
    );
}

#[gpui::test]
async fn no_host_key_prompter_is_not_reported_as_a_refusal(cx: &mut TestAppContext) {
    let host_keys = SharedHostKeys::default();
    let offered = unknown_host_key("SHA256:abc123");
    let expected = format!("{offered}; no prompt is available to approve it");
    let script =
        Script::new(vec![Some(ssh_upstream("git.example.com"))]).fetches(vec![Err(offered)]);
    let (model, _) = with_host_keys(cx, script, &host_keys);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(failed_message(&state(cx, &model)).1, expected);
    assert!(host_keys.stored().is_empty());
}

#[gpui::test]
async fn divergence_stops_the_sync_before_push(cx: &mut TestAppContext) {
    let script = Script::new(vec![Some(upstream("example.com", 1, 1))])
        .fetches(vec![Ok(fetched(1))])
        .pulls(vec![Err(ActionError::Diverged {
            branch: "main".to_owned(),
            ahead: 1,
            behind: 1,
        })]);
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;
    let SyncState::Diverged(message) = state(cx, &model) else {
        panic!("expected diverged");
    };
    assert!(message.contains("fast-forward"), "{message}");
    assert_eq!(
        stub.state().push_calls,
        0,
        "a diverged pull must stop the push"
    );
}

#[gpui::test]
async fn push_alone_runs_no_fetch_or_pull(cx: &mut TestAppContext) {
    let script = Script::new(vec![Some(upstream("example.com", 1, 0))]).pushes(vec![Ok(pushed(1))]);
    let (model, stub) = fixture(cx, script);
    run(cx, &model, SyncModel::push).await;
    assert!(
        stub.state().fetch_requests.is_empty(),
        "push must not fetch first"
    );
    assert_eq!(stub.state().pull_calls, 0);
    assert_eq!(stub.state().push_calls, 1);
    assert_eq!(state(cx, &model), done(0, 0, 1));
}

#[gpui::test]
async fn pull_fetches_then_fast_forwards_and_never_pushes(cx: &mut TestAppContext) {
    let script = Script::new(vec![Some(upstream("example.com", 0, 1))])
        .fetches(vec![Ok(fetched(1))])
        .pulls(vec![Ok(pulled(2))]);
    let (model, stub) = fixture(cx, script);
    run(cx, &model, SyncModel::pull).await;
    assert_eq!(stub.state().fetch_requests.len(), 1);
    assert_eq!(stub.state().pull_calls, 1);
    assert_eq!(stub.state().push_calls, 0);
    assert_eq!(state(cx, &model), done(1, 2, 0));
}

#[gpui::test]
async fn a_rejected_push_ends_diverged_not_failed(cx: &mut TestAppContext) {
    let rejected = ActionError::PushRejected {
        branch: "main".to_owned(),
    };
    let expected = rejected.to_string();
    let script = Script::new(vec![Some(upstream("example.com", 1, 0))])
        .fetches(vec![Ok(fetched(1))])
        .pulls(vec![Ok(pulled(2))])
        .pushes(vec![Err(rejected)]);
    let (model, stub) = fixture(cx, script);
    run(cx, &model, SyncModel::sync).await;
    assert_eq!(stub.state().push_calls, 1);
    assert_eq!(state(cx, &model), SyncState::Diverged(expected));
}

#[gpui::test]
async fn no_upstream_fails_fast_without_network(cx: &mut TestAppContext) {
    let (model, stub) = fixture(cx, Script::new(vec![Some(no_upstream("main"))]));
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;
    assert_eq!(
        failed_message(&state(cx, &model)).1,
        "branch 'main' has no remote to use; add an 'origin' remote or set branch.main.remote"
    );
    assert!(
        stub.state().fetch_requests.is_empty(),
        "no network step may run"
    );
}

#[gpui::test]
async fn a_second_workflow_while_busy_is_a_no_op(cx: &mut TestAppContext) {
    let (model, stub) = fixture(cx, full_sync(Some(upstream("example.com", 0, 0))));
    refresh(cx, &model).await;
    let first = model.update(cx, SyncModel::sync);
    let second = model.update(cx, SyncModel::sync);
    second.await;
    first.await;
    assert_eq!(
        stub.state().fetch_requests.len(),
        1,
        "the overlapping sync never reached the network"
    );
    assert_eq!(stub.state().push_calls, 1);
    assert_eq!(state(cx, &model), done(1, 2, 1));
}

#[gpui::test]
async fn an_up_to_date_sync_reports_zeros(cx: &mut TestAppContext) {
    let script = Script::upstream()
        .fetches(vec![Ok(fetched(0))])
        .pulls(vec![Ok(pulled(0))])
        .pushes(vec![Ok(pushed(0))]);
    let (model, _) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;
    assert_eq!(state(cx, &model), done(0, 0, 0));
}

#[gpui::test]
async fn reset_returns_a_terminal_state_to_idle(cx: &mut TestAppContext) {
    let script = Script::upstream().fetches(vec![Err(failure("boom"))]);
    let (model, _) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert!(matches!(state(cx, &model), SyncState::Failed { .. }));
    model.update(cx, SyncModel::reset);
    assert_eq!(state(cx, &model), SyncState::Idle);
}

fn host_key_script(
    fetches: Vec<Result<rc_git_guard::actions::FetchOutcome, ActionError>>,
) -> Script {
    Script::new(vec![Some(ssh_upstream("git.example.com"))]).fetches(fetches)
}

#[gpui::test]
async fn an_unknown_host_key_prompts_once_persists_and_retries(cx: &mut TestAppContext) {
    let host_keys = SharedHostKeys::default();
    let script = host_key_script(vec![Err(unknown_host_key("SHA256:abc123")), Ok(fetched(1))]);
    let (model, stub) = with_host_keys(cx, script, &host_keys);
    let asked = answer_host_keys(cx, &model, HostKeyReply::Trusted);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(asked.borrow()[0].host_key, "ssh-ed25519 SHA256:abc123");
    assert!(matches!(state(cx, &model), SyncState::Done(_)));
    assert_eq!(
        stub.fetch_host_keys(),
        [None, Some("SHA256:abc123".to_owned())]
    );
    assert_eq!(
        host_keys.stored(),
        [("git.example.com".to_owned(), "SHA256:abc123".to_owned())]
    );
}

#[gpui::test]
async fn a_host_key_still_unknown_after_trust_fails_without_a_second_prompt(
    cx: &mut TestAppContext,
) {
    let still = unknown_host_key("SHA256:abc123");
    let expected = still.to_string();
    let script = host_key_script(vec![Err(unknown_host_key("SHA256:abc123")), Err(still)]);
    let (model, stub) = fixture(cx, script);
    let asked = answer_host_keys(cx, &model, HostKeyReply::Trusted);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(
        asked.borrow().len(),
        1,
        "a key the guard still rejects after trust must not ask again"
    );
    assert_eq!(
        stub.fetch_host_keys(),
        [None, Some("SHA256:abc123".to_owned())]
    );
    assert_eq!(
        state(cx, &model),
        SyncState::Failed {
            step: SyncStep::Fetch,
            message: expected
        }
    );
}

#[gpui::test]
async fn declining_the_host_key_fails_the_step(cx: &mut TestAppContext) {
    let host_keys = SharedHostKeys::default();
    let script = host_key_script(vec![Err(unknown_host_key("SHA256:abc123"))]);
    let (model, _) = with_host_keys(cx, script, &host_keys);
    answer_host_keys(cx, &model, HostKeyReply::Refused);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert!(failed_message(&state(cx, &model)).1.contains("not trusted"));
    assert!(
        host_keys.stored().is_empty(),
        "a declined key must not be stored"
    );
}

#[gpui::test]
async fn a_stored_fingerprint_skips_the_prompt(cx: &mut TestAppContext) {
    let host_keys = SharedHostKeys::with(&[("git.example.com", "SHA256:abc123")]);
    let (model, stub) = with_host_keys(cx, host_key_script(vec![Ok(fetched(1))]), &host_keys);
    let asked = answer_host_keys(cx, &model, HostKeyReply::Trusted);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert!(
        asked.borrow().is_empty(),
        "a stored fingerprint is passed without asking"
    );
    assert_eq!(stub.fetch_host_keys(), [Some("SHA256:abc123".to_owned())]);
}

#[gpui::test]
async fn a_changed_host_key_fails_without_prompting(cx: &mut TestAppContext) {
    let host_keys = SharedHostKeys::with(&[("git.example.com", "SHA256:abc123")]);
    let script = host_key_script(vec![Err(unknown_host_key("SHA256:other"))]);
    let (model, stub) = with_host_keys(cx, script, &host_keys);
    let asked = answer_host_keys(cx, &model, HostKeyReply::Trusted);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert!(
        asked.borrow().is_empty(),
        "a changed key must not reach the first-use sheet"
    );
    let message = failed_message(&state(cx, &model)).1;
    assert!(message.contains("SHA256:abc123"), "{message}");
    assert!(message.contains("SHA256:other"), "{message}");
    assert_eq!(stub.fetch_host_keys(), [Some("SHA256:abc123".to_owned())]);
    assert_eq!(
        host_keys.stored(),
        [("git.example.com".to_owned(), "SHA256:abc123".to_owned())]
    );
}

#[gpui::test]
async fn a_removed_approval_makes_the_rotated_key_unknown_again(cx: &mut TestAppContext) {
    let host_keys = SharedHostKeys::with(&[("git.example.com", "SHA256:abc123")]);
    let script = host_key_script(vec![
        Err(unknown_host_key("SHA256:rotated")),
        Ok(fetched(1)),
    ]);
    let (model, stub) = with_host_keys(cx, script, &host_keys);
    let mut settings = ApprovedHostsModel::new(host_keys.clone());
    assert_eq!(settings.remove(&[0]), Ok(true));
    assert!(settings.rows().is_empty());
    let asked = answer_host_keys(cx, &model, HostKeyReply::Trusted);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(
        asked.borrow()[0].host_key,
        "ssh-ed25519 SHA256:rotated",
        "a forgotten host is first contact"
    );
    assert!(matches!(state(cx, &model), SyncState::Done(_)));
    assert_eq!(
        stub.fetch_host_keys(),
        [None, Some("SHA256:rotated".to_owned())]
    );
    assert_eq!(
        host_keys.stored(),
        [("git.example.com".to_owned(), "SHA256:rotated".to_owned())]
    );
}

#[gpui::test]
async fn an_ssh_passphrase_prompt_retries_and_stores_under_the_ssh_kind(cx: &mut TestAppContext) {
    let store = TestStore::default();
    let script = host_key_script(vec![
        Err(passphrase_required("/k/id_ed25519")),
        Ok(fetched(1)),
    ]);
    let (model, stub) = with_store(cx, script, &store);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("", "opensesame")),
    );
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(asked.borrow()[0].kind, SSH);
    assert_eq!(
        asked.borrow()[0].key.as_deref(),
        Some("/k/id_ed25519"),
        "the prompt names the locked key"
    );
    assert!(matches!(state(cx, &model), SyncState::Done(_)));
    assert_eq!(
        stub.fetch_tokens().last(),
        Some(&Some("opensesame".to_owned()))
    );
    assert_eq!(
        store.stored("git.example.com", SSH).as_deref(),
        Some("opensesame")
    );
    assert_eq!(store.stored("git.example.com", HTTPS), None);
}

/// Over ssh, an auth failure means every key was offered and rejected: no passphrase can fix
/// that, so the step fails with the server's answer instead of asking.
#[gpui::test]
async fn an_ssh_auth_rejection_fails_at_once_without_a_prompt(cx: &mut TestAppContext) {
    let store = TestStore::default();
    let rejected = auth_required("git.example.com");
    let expected = rejected.to_string();
    let script = host_key_script(vec![Err(rejected), Ok(fetched(1))]);
    let (model, stub) = with_store(cx, script, &store);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("", "opensesame")),
    );
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert!(
        asked.borrow().is_empty(),
        "a rejected key is not a locked key"
    );
    assert_eq!(
        state(cx, &model),
        SyncState::Failed {
            step: SyncStep::Fetch,
            message: expected
        }
    );
    assert_eq!(stub.state().fetch_requests.len(), 1, "no retry");
    assert_eq!(store.len(), 0);
}

/// A redirect to another host that asks for credentials gets none from rc-git, so asking
/// again cannot help: the step fails at once with the guard's message.
#[gpui::test]
async fn a_refused_credential_host_fails_at_once_without_a_prompt(cx: &mut TestAppContext) {
    let store = TestStore::default();
    let refusal = ActionError::CredentialHostRefused {
        op: "fetch",
        host: "example.com".to_owned(),
        requested: "login.example.net".to_owned(),
    };
    let expected = refusal.to_string();
    let script = Script::upstream().fetches(vec![Err(refusal), Ok(fetched(1))]);
    let (model, stub) = with_store(cx, script, &store);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("me", "typed")),
    );
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert!(
        asked.borrow().is_empty(),
        "no credentials can satisfy another host"
    );
    assert_eq!(
        state(cx, &model),
        SyncState::Failed {
            step: SyncStep::Fetch,
            message: expected
        }
    );
    assert_eq!(stub.state().fetch_requests.len(), 1, "no retry");
    assert_eq!(store.len(), 0);
}

#[gpui::test]
async fn an_scp_like_remote_asks_for_the_passphrase_and_leaves_the_https_token_alone(
    cx: &mut TestAppContext,
) {
    let store = TestStore::with(&[("example.com", HTTPS, credentials("me", "pat-for-https"))]);
    let mut scp_like = ssh_upstream("example.com");
    scp_like.url = "example.com:x/y.git".to_owned();
    let script = Script::new(vec![Some(scp_like)]).fetches(vec![
        Err(passphrase_required("/k/id_ed25519")),
        Ok(fetched(1)),
    ]);
    let (model, stub) = with_store(cx, script, &store);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("", "opensesame")),
    );
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(asked.borrow()[0].kind, SSH);
    assert_eq!(asked.borrow()[0].key.as_deref(), Some("/k/id_ed25519"));
    assert!(
        stub.fetch_tokens()
            .iter()
            .all(|token| token.as_deref() != Some("pat-for-https")),
        "the HTTPS token must never be offered as the key passphrase"
    );
    assert_eq!(
        store.stored("example.com", SSH).as_deref(),
        Some("opensesame")
    );
    assert_eq!(
        store.stored("example.com", HTTPS).as_deref(),
        Some("pat-for-https")
    );
}

/// Checks a cancelled workflow left nothing behind: the model is idle and not busy.
fn assert_idle(cx: &mut TestAppContext, model: &Entity<SyncModel>) {
    assert_eq!(
        state(cx, model),
        SyncState::Idle,
        "a cancel is not a failure to report"
    );
    assert!(!model.read_with(cx, |m, _| m.is_busy()));
}

#[gpui::test]
async fn cancelling_mid_step_ends_idle_without_an_error_or_the_remaining_steps(
    cx: &mut TestAppContext,
) {
    let mut script = full_sync(Some(upstream("example.com", 1, 0)));
    script.fetches = vec![Err(ActionError::Cancelled { op: "fetch" })].into();
    let (model, stub) = fixture(cx, script);
    // The real transfer sees the flag and fails; the stub raises the token in the step.
    stub.state().signal_in_step = true;
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;
    assert_idle(cx, &model);
    assert_eq!(
        stub.state().pull_calls,
        0,
        "a cancelled fetch must stop the workflow"
    );
    assert_eq!(stub.state().push_calls, 0);
}

#[gpui::test]
async fn a_cancel_during_a_fetch_that_succeeds_still_skips_the_pull(cx: &mut TestAppContext) {
    let script = Script::new(vec![Some(upstream("example.com", 0, 1))])
        .fetches(vec![Ok(fetched(1))])
        .pulls(vec![Ok(pulled(2))]);
    let (model, stub) = fixture(cx, script);
    // The stop lands after the transfer's last poll, so the fetch still succeeds.
    stub.state().signal_in_step = true;
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::pull).await;
    assert_idle(cx, &model);
    assert_eq!(
        stub.state().pull_calls,
        0,
        "the user said stop before the working tree moved"
    );
}

#[gpui::test]
async fn a_cancelled_workflow_never_offers_stop_again(cx: &mut TestAppContext) {
    let script = Script::new(vec![Some(upstream("example.com", 0, 1))])
        .fetches(vec![Ok(fetched(1))])
        .pulls(vec![Ok(pulled(2))]);
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    let seen = states(cx, &model);
    let cancels = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(&model, move |model, event: &SyncEvent, cx| {
            let first = cancels.borrow().len() == 1;
            if first && *event == SyncEvent::StateChanged(SyncState::Running(SyncStep::Fetch)) {
                model.update(cx, SyncModel::cancel_running);
            }
        })
        .detach();
    });
    run(cx, &model, SyncModel::pull).await;

    let seen = seen.borrow();
    let cancelled_at = seen
        .iter()
        .position(|state| *state == SyncState::Cancelling(SyncStep::Fetch));
    assert!(cancelled_at.is_some(), "the cancel must land: {seen:?}");
    let after = &seen[cancelled_at.unwrap_or_default()..];
    assert!(
        !after
            .iter()
            .any(|state| matches!(state, SyncState::Running(_))),
        "a step entered after Stop must stay cancelling: {seen:?}"
    );
    drop(seen);
    assert_idle(cx, &model);
    assert!(stub.state().fetch_requests.is_empty());
    assert_eq!(stub.state().pull_calls, 0);
}

#[gpui::test]
async fn cancelling_is_refused_when_nothing_runs_and_the_next_sync_still_works(
    cx: &mut TestAppContext,
) {
    let (model, stub) = fixture(cx, full_sync(Some(upstream("example.com", 0, 0))));
    refresh(cx, &model).await;
    model.update(cx, SyncModel::cancel_running);
    assert_eq!(state(cx, &model), SyncState::Idle);
    assert!(!model.read_with(cx, |m, _| m.is_cancellable()));
    // A cancel while idle is refused, so it leaves nothing behind for the next sync.
    run(cx, &model, SyncModel::sync).await;
    assert_eq!(state(cx, &model), done(1, 2, 1));
    assert_eq!(stub.state().push_calls, 1);
}

#[gpui::test]
async fn cancel_running_mid_fetch_stops_the_workflow(cx: &mut TestAppContext) {
    let mut script = full_sync(Some(upstream("example.com", 1, 0)));
    script.fetches = vec![Err(ActionError::Cancelled { op: "fetch" })].into();
    let (model, stub) = fixture(cx, script);
    let seen = states(cx, &model);
    refresh(cx, &model).await;
    let held = stub.hold(Step::Fetch);
    let sync = model.update(cx, SyncModel::sync);
    held.arrived.await;
    // Stop the way the toolbar does: through the model, which is free while the step runs.
    let offered = model.read_with(cx, |m, _| m.is_cancellable());
    model.update(cx, SyncModel::cancel_running);
    held.gate.open();
    sync.await;

    assert!(offered, "a running fetch must offer Stop");
    assert_eq!(
        stub.state().tokens_signalled,
        [true],
        "cancel_running must raise the running step's token"
    );
    assert!(
        seen.borrow()
            .contains(&SyncState::Cancelling(SyncStep::Fetch))
    );
    assert_idle(cx, &model);
    assert_eq!(
        stub.state().pull_calls,
        0,
        "a cancelled fetch must stop the workflow"
    );
    assert_eq!(stub.state().push_calls, 0);

    // The flag a cancel raises belongs to one workflow: the next one starts clean.
    stub.state().queue_fetch(Ok(fetched(1)));
    run(cx, &model, SyncModel::sync).await;
    assert_eq!(state(cx, &model), done(1, 2, 1));
}

#[gpui::test]
async fn a_cancel_while_the_credential_sheet_is_up_starts_no_second_attempt(
    cx: &mut TestAppContext,
) {
    let store = TestStore::default();
    let script =
        Script::upstream().fetches(vec![Err(auth_required("example.com")), Ok(fetched(1))]);
    let (model, stub) = with_store(cx, script, &store);
    let weak = model.downgrade();
    model.update(cx, |m, _| {
        m.set_credential_prompter(Rc::new(move |_, answer, cx| {
            let _ = weak.update(cx, SyncModel::cancel_running);
            answer.send(CredentialReply::Provided(credentials("user", "typed")));
        }));
    });
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(
        stub.state().fetch_requests.len(),
        1,
        "the answer after Stop is not tried"
    );
    assert_idle(cx, &model);
    assert_eq!(store.len(), 0, "credentials never tried are never stored");
}

#[gpui::test]
async fn a_cancelled_workflow_leaves_the_next_one_clean(cx: &mut TestAppContext) {
    let mut script = full_sync(Some(upstream("example.com", 0, 0)));
    script
        .fetches
        .push_front(Err(ActionError::Cancelled { op: "fetch" }));
    let (model, stub) = fixture(cx, script);
    stub.state().signal_in_step = true;
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;
    assert_idle(cx, &model);

    stub.state().signal_in_step = false;
    run(cx, &model, SyncModel::sync).await;
    assert_eq!(state(cx, &model), done(1, 2, 1));
    assert_eq!(stub.state().push_calls, 1);
}

#[gpui::test]
async fn a_cancelled_auth_failure_does_not_reopen_the_credential_prompt(cx: &mut TestAppContext) {
    let script = Script::upstream().fetches(vec![Err(auth_required("example.com"))]);
    let (model, stub) = fixture(cx, script);
    let asked = answer_credentials(
        cx,
        &model,
        CredentialReply::Provided(credentials("user", "token")),
    );
    stub.state().signal_in_step = true;
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert!(asked.borrow().is_empty(), "the user already said stop");
    assert_idle(cx, &model);
    assert_eq!(
        stub.state().fetch_requests.len(),
        1,
        "no retry after a cancel"
    );
}

/// Cancels through the model while `step`, which takes no token, is held, after checking the
/// toolbar does not offer Stop there.
async fn cancel_during_local_step(
    cx: &mut TestAppContext,
    model: &Entity<SyncModel>,
    stub: &Stub,
    step: Step,
    workflow: fn(&mut SyncModel, &mut gpui::Context<SyncModel>) -> gpui::Task<()>,
) {
    let held = stub.hold(step);
    let running = model.update(cx, workflow);
    held.arrived.await;
    let shown = state(cx, model);
    let offered = model.read_with(cx, |m, _| m.is_cancellable());
    model.update(cx, SyncModel::cancel_running);
    held.gate.open();
    running.await;
    assert!(matches!(shown, SyncState::Running(_)), "{shown:?}");
    assert!(
        !offered,
        "a {step:?} cannot stop; the toolbar must not offer it"
    );
}

#[gpui::test]
async fn a_cancel_during_the_switch_still_reports_it_and_skips_the_network(
    cx: &mut TestAppContext,
) {
    let script = Script::upstream()
        .fetches(vec![Ok(fetched(1))])
        .pulls(vec![Ok(pulled(2))])
        .checkouts(vec![Ok(switched(false, true))]);
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    cancel_during_local_step(
        cx,
        &model,
        &stub,
        Step::Checkout,
        SyncModel::back_to_default_branch,
    )
    .await;
    assert_eq!(
        state(cx, &model),
        SyncState::Done(SyncSummary {
            pulled_files: 4,
            cancelled_after: Some(SyncStep::Checkout),
            ..SyncSummary::default()
        })
    );
    assert_eq!(
        model.update(cx, |m, _| m.take_switched_branch()).as_deref(),
        Some("main"),
        "the switch landed, so the browser must still follow it"
    );
    assert!(
        stub.state().fetch_requests.is_empty(),
        "the cancel stops the workflow after it"
    );
    assert_eq!(stub.state().pull_calls, 0);
}

#[gpui::test]
async fn a_cancel_during_the_pull_keeps_its_files_and_skips_the_push(cx: &mut TestAppContext) {
    let (model, stub) = fixture(cx, full_sync(Some(upstream("example.com", 1, 1))));
    refresh(cx, &model).await;
    cancel_during_local_step(cx, &model, &stub, Step::Pull, SyncModel::sync).await;
    assert_eq!(
        state(cx, &model),
        SyncState::Done(SyncSummary {
            fetched_refs: 1,
            pulled_files: 2,
            cancelled_after: Some(SyncStep::Pull),
            ..SyncSummary::default()
        })
    );
    assert_eq!(
        stub.state().push_calls,
        0,
        "the cancel stops the workflow before the push"
    );
    assert!(!model.read_with(cx, |m, _| m.is_busy()));
}

#[gpui::test]
async fn a_cancel_before_the_switch_starts_switches_nothing(cx: &mut TestAppContext) {
    let script = Script::upstream().checkouts(vec![Ok(switched(false, true))]);
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    let cancelled = Rc::new(std::cell::Cell::new(false));
    let once = Rc::clone(&cancelled);
    cx.update(|cx| {
        cx.subscribe(&model, move |model, event: &SyncEvent, cx| {
            if !once.get()
                && *event == SyncEvent::StateChanged(SyncState::Running(SyncStep::Checkout))
            {
                once.set(true);
                model.update(cx, SyncModel::cancel_running);
            }
        })
        .detach();
    });
    run(cx, &model, SyncModel::back_to_default_branch).await;
    assert!(cancelled.get());
    assert_idle(cx, &model);
    assert!(
        stub.state().checkout_branches.is_empty(),
        "a cancel before the write runs it never"
    );
}

fn back_to_default_script(
    checkout: Result<rc_git_guard::actions::CheckoutOutcome, ActionError>,
) -> Script {
    Script::upstream()
        .fetches(vec![Ok(fetched(1))])
        .pulls(vec![Ok(pulled(2))])
        .checkouts(vec![checkout])
}

#[gpui::test]
async fn back_to_default_switches_then_fetches_and_fast_forwards(cx: &mut TestAppContext) {
    let (model, stub) = fixture(cx, back_to_default_script(Ok(switched(false, true))));
    let seen = states(cx, &model);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::back_to_default_branch).await;

    assert_eq!(stub.state().checkout_branches, ["main"]);
    let steps: Vec<SyncStep> = seen
        .borrow()
        .iter()
        .filter_map(|state| match state {
            SyncState::Running(step) => Some(*step),
            _ => None,
        })
        .collect();
    assert_eq!(steps.first(), Some(&SyncStep::Checkout));
    let network: Vec<_> = steps
        .iter()
        .filter(|step| **step != SyncStep::Checkout)
        .collect();
    assert_eq!(network, [&SyncStep::Fetch, &SyncStep::Pull], "{steps:?}");
    assert_eq!(stub.state().pull_calls, 1);
    assert_eq!(
        state(cx, &model),
        done(1, 6, 0),
        "the files the switch wrote and the ones the pull wrote both count"
    );
    assert_eq!(
        model.update(cx, |m, _| m.take_switched_branch()).as_deref(),
        Some("main")
    );
    assert_eq!(
        model.update(cx, |m, _| m.take_switched_branch()),
        None,
        "followed once"
    );
}

#[gpui::test]
async fn a_switch_that_missed_its_reflog_line_finishes_with_its_warning(cx: &mut TestAppContext) {
    let outcome = switched(false, false);
    let warning = checkout_warning(&outcome).unwrap_or_default();
    assert!(warning.contains("git checkout -"));
    let (model, _) = fixture(cx, back_to_default_script(Ok(outcome)));
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::back_to_default_branch).await;
    assert_eq!(
        state(cx, &model),
        SyncState::Done(SyncSummary {
            notice: Some(warning),
            ..summary(1, 6, 0)
        }),
        "the switch landed, so the workflow finishes and carries the warning"
    );
    assert_eq!(
        model.update(cx, |m, _| m.take_switched_branch()).as_deref(),
        Some("main")
    );
}

#[test]
fn a_switch_with_its_reflog_line_has_no_warning() {
    assert_eq!(checkout_warning(&switched(false, true)), None);
    assert_eq!(checkout_warning(&switched(true, true)), None);
}

#[gpui::test]
async fn a_push_that_left_its_tracking_ref_finishes_with_the_warning(cx: &mut TestAppContext) {
    let mut stuck = pushed(1);
    stuck.tracking_ref_error = Some("lock held".to_owned());
    let script = Script::new(vec![Some(upstream("example.com", 1, 0))]).pushes(vec![Ok(stuck)]);
    let (model, _) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::push).await;
    assert_eq!(
        state(cx, &model),
        SyncState::Done(SyncSummary {
            notice: Some("push landed, but the tracking ref was not moved: lock held".to_owned()),
            ..summary(0, 0, 1)
        }),
        "the push landed, so it is done, with the stale tracking ref as a notice"
    );
}

#[gpui::test]
async fn a_sync_whose_push_left_its_tracking_ref_finishes_with_the_warning(
    cx: &mut TestAppContext,
) {
    let mut stuck = pushed(1);
    stuck.tracking_ref_error = Some("lock held".to_owned());
    let mut script = full_sync(Some(upstream("example.com", 0, 0)));
    script.pushes = vec![Ok(stuck)].into();
    let (model, _) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::sync).await;
    assert_eq!(
        state(cx, &model),
        SyncState::Done(SyncSummary {
            notice: Some("push landed, but the tracking ref was not moved: lock held".to_owned()),
            ..summary(1, 2, 1)
        })
    );
}

#[gpui::test]
async fn back_to_default_on_the_branch_already_still_fetches_but_reports_no_switch(
    cx: &mut TestAppContext,
) {
    let (model, stub) = fixture(cx, back_to_default_script(Ok(switched(true, true))));
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::back_to_default_branch).await;
    assert_eq!(stub.state().pull_calls, 1);
    assert_eq!(
        state(cx, &model),
        done(1, 2, 0),
        "nothing was switched, so nothing to follow in the UI"
    );
    assert_eq!(model.update(cx, |m, _| m.take_switched_branch()), None);
}

#[gpui::test]
async fn a_refused_switch_stops_before_any_network_step(cx: &mut TestAppContext) {
    let refused = ActionError::DirtyPath {
        op: "checkout",
        path: "alpha.txt".to_owned(),
    };
    let (model, stub) = fixture(cx, back_to_default_script(Err(refused)));
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::back_to_default_branch).await;
    let (step, message) = failed_message(&state(cx, &model));
    assert_eq!(step, SyncStep::Checkout);
    assert!(message.contains("alpha.txt"), "{message}");
    assert!(
        stub.state().fetch_requests.is_empty(),
        "a refused switch may not reach the network"
    );
    assert_eq!(stub.state().pull_calls, 0);
}

#[gpui::test]
async fn no_mainline_branch_fails_before_the_switch(cx: &mut TestAppContext) {
    let mut script = Script::upstream();
    script.default_branch = None;
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::back_to_default_branch).await;
    let (step, message) = failed_message(&state(cx, &model));
    assert_eq!(step, SyncStep::Checkout);
    assert!(message.contains("mainline"), "{message}");
    assert!(stub.state().checkout_branches.is_empty());
}

#[gpui::test]
async fn the_branch_switched_to_without_an_upstream_stops_before_the_fetch(
    cx: &mut TestAppContext,
) {
    let mut script = back_to_default_script(Ok(switched(false, true)));
    script.preflights = vec![
        Some(upstream("example.com", 0, 0)),
        Some(no_upstream("main")),
    ];
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::back_to_default_branch).await;
    assert_eq!(
        stub.state().checkout_branches,
        ["main"],
        "the switch is local and still runs"
    );
    assert_eq!(
        state(cx, &model),
        SyncState::Failed {
            step: SyncStep::Fetch,
            message: "branch 'main' has no remote to use; add an 'origin' remote or set \
                      branch.main.remote"
                .to_owned()
        }
    );
    assert!(
        stub.state().fetch_requests.is_empty(),
        "no network step may run"
    );
    assert_eq!(
        model.update(cx, |m, _| m.take_switched_branch()).as_deref(),
        Some("main"),
        "HEAD moved before the failure, so the browser must still follow it"
    );
}

#[gpui::test]
async fn a_fetch_that_fails_after_the_switch_still_reports_the_switch(cx: &mut TestAppContext) {
    let mut script = back_to_default_script(Ok(switched(false, true)));
    script.fetches = vec![Err(failure("offline"))].into();
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::back_to_default_branch).await;
    let (step, message) = failed_message(&state(cx, &model));
    assert_eq!(step, SyncStep::Fetch);
    assert!(message.contains("offline"));
    assert_eq!(stub.state().pull_calls, 0);
    assert_eq!(
        model.update(cx, |m, _| m.take_switched_branch()).as_deref(),
        Some("main")
    );
}

#[gpui::test]
async fn a_new_workflow_forgets_a_switch_nobody_followed(cx: &mut TestAppContext) {
    let mut script = back_to_default_script(Ok(switched(false, true)));
    script.fetches = vec![Err(failure("offline")), Ok(fetched(1))].into();
    let (model, _) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::back_to_default_branch).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(state(cx, &model), done(1, 0, 0));
    assert_eq!(
        model.update(cx, |m, _| m.take_switched_branch()),
        None,
        "the fetch switched nothing"
    );
}

#[gpui::test]
async fn a_failed_refresh_clears_the_stale_status_and_says_why(cx: &mut TestAppContext) {
    let (model, _) = fixture(
        cx,
        Script::new(vec![Some(upstream("example.com", 2, 0)), None]),
    );
    refresh(cx, &model).await;
    assert_eq!(
        model.read_with(cx, |m, _| push_title(m.remote_status())),
        "Push 2 Commits"
    );
    let changes = Rc::new(std::cell::Cell::new(0));
    let counter = Rc::clone(&changes);
    cx.update(|cx| {
        cx.subscribe(&model, move |_, event: &SyncEvent, _| {
            if *event == SyncEvent::StatusChanged {
                counter.set(counter.get() + 1);
            }
        })
        .detach();
    });
    refresh(cx, &model).await;

    assert_eq!(model.read_with(cx, |m, _| m.remote_status().cloned()), None);
    assert_eq!(
        model
            .read_with(cx, |m, _| m.status_error().map(str::to_owned))
            .as_deref(),
        Some("preflight failed")
    );
    assert_eq!(changes.get(), 1, "the views hear about the failure");
    assert_eq!(
        model.read_with(cx, |m, _| push_title(m.remote_status())),
        "Push"
    );
    let availability = model.read_with(cx, |m, _| m.availability(true));
    assert!(!availability.can_pull && !availability.can_push && !availability.can_sync);
    assert_eq!(
        model.read_with(cx, |m, _| rc_git_kit::sync::idle_sync_tooltip(
            m.remote_status(),
            m.status_error()
        )),
        "Sync — couldn't read upstream status: preflight failed"
    );
}

#[gpui::test]
async fn a_successful_refresh_after_a_failure_clears_the_error(cx: &mut TestAppContext) {
    let (model, _) = fixture(
        cx,
        Script::new(vec![None, Some(upstream("example.com", 0, 0))]),
    );
    refresh(cx, &model).await;
    assert!(model.read_with(cx, |m, _| m.status_error().is_some()));
    refresh(cx, &model).await;
    assert!(model.read_with(cx, |m, _| m.status_error().is_none()));
    assert_eq!(
        model.read_with(cx, |m, _| m.remote_status().cloned()),
        Some(upstream("example.com", 0, 0))
    );
}

#[gpui::test]
async fn a_slow_refresh_does_not_overwrite_the_fresh_read_a_workflow_made(cx: &mut TestAppContext) {
    let script = Script::new(vec![
        Some(upstream("example.com", 1, 0)),
        Some(upstream("example.com", 2, 0)),
    ])
    .fetches(vec![Ok(fetched(1))]);
    let (model, stub) = fixture(cx, script);
    let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(&model, move |model, event: &SyncEvent, cx| {
            if *event == SyncEvent::StatusChanged {
                sink.borrow_mut()
                    .push(model.read(cx).remote_status().map(|status| status.ahead));
            }
        })
        .detach();
    });
    let held = stub.hold(Step::Preflight);
    let slow = model.update(cx, SyncModel::refresh_status);
    held.arrived.await;
    run(cx, &model, SyncModel::fetch).await;
    held.gate.open();
    slow.await;

    assert_eq!(
        model.read_with(cx, |m, _| m.remote_status().map(|s| s.ahead)),
        Some(2)
    );
    assert!(
        !seen.borrow().contains(&Some(1)),
        "the older answer never lands: {seen:?}"
    );
}

#[gpui::test]
async fn a_burst_of_refreshes_costs_two_reads_and_one_mainline_read(cx: &mut TestAppContext) {
    let mut script = Script::upstream();
    script.default_branch = None;
    let (model, stub) = fixture(cx, script);
    let held = stub.hold(Step::Preflight);
    let slow = model.update(cx, SyncModel::refresh_status);
    held.arrived.await;
    for _ in 0..20 {
        refresh(cx, &model).await;
    }
    held.gate.open();
    slow.await;
    assert_eq!(
        stub.state().preflight_calls,
        2,
        "the burst joins one more read"
    );

    refresh(cx, &model).await;
    refresh(cx, &model).await;
    assert_eq!(
        stub.state().default_branch_calls,
        1,
        "a repository that names no mainline is not asked again"
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.default_branch().map(str::to_owned)),
        None
    );
}

#[gpui::test]
async fn an_inexact_mainline_is_refused_before_the_switch(cx: &mut TestAppContext) {
    for refresh_first in [true, false] {
        let mut script = Script::upstream();
        script.default_branch = Some(DefaultBranch {
            name: "tr\u{FFFD}".to_owned(),
            exact_name: false,
        });
        let (model, stub) = fixture(cx, script);
        if refresh_first {
            refresh(cx, &model).await;
            assert_eq!(
                model
                    .read_with(cx, |m, _| m.default_branch().map(str::to_owned))
                    .as_deref(),
                Some("tr\u{FFFD}"),
                "the name still titles the control"
            );
        }
        run(cx, &model, SyncModel::back_to_default_branch).await;
        assert_eq!(
            state(cx, &model),
            SyncState::Failed {
                step: SyncStep::Checkout,
                message: "cannot switch to 'tr\u{FFFD}': the branch name is not valid UTF-8"
                    .to_owned()
            },
            "refresh first: {refresh_first}"
        );
        assert!(
            stub.state().checkout_branches.is_empty(),
            "the repaired name is never checked out"
        );
        assert!(stub.state().fetch_requests.is_empty());
    }
}

#[gpui::test]
async fn a_failed_mainline_read_is_retried_on_the_next_refresh(cx: &mut TestAppContext) {
    let mut script = Script::upstream();
    script.default_branch_fails = true;
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    refresh(cx, &model).await;
    assert_eq!(
        stub.state().default_branch_calls,
        2,
        "a failure is not an answer"
    );
}

#[gpui::test]
async fn an_unreadable_mainline_names_the_cause(cx: &mut TestAppContext) {
    let mut script = Script::upstream();
    script.default_branch_fails = true;
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::back_to_default_branch).await;
    assert_eq!(
        state(cx, &model),
        SyncState::Failed {
            step: SyncStep::Checkout,
            message: "cannot read the mainline branch: cannot open /tmp/repo".to_owned()
        }
    );
    assert!(stub.state().checkout_branches.is_empty());
}

#[gpui::test]
async fn an_unreadable_upstream_after_the_switch_stops_before_the_fetch(cx: &mut TestAppContext) {
    let mut script = back_to_default_script(Ok(switched(false, true)));
    script.preflights = vec![Some(upstream("example.com", 0, 0)), None];
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::back_to_default_branch).await;
    assert_eq!(
        state(cx, &model),
        SyncState::Failed {
            step: SyncStep::Fetch,
            message: "cannot read the upstream status: preflight failed".to_owned()
        }
    );
    assert_eq!(stub.state().checkout_branches, ["main"]);
    assert!(
        stub.state().fetch_requests.is_empty(),
        "no network step may run"
    );
}

#[gpui::test]
async fn an_unreadable_upstream_refuses_the_fetch_with_the_cause(cx: &mut TestAppContext) {
    let script =
        Script::new(vec![Some(upstream("example.com", 0, 0)), None]).fetches(vec![Ok(fetched(1))]);
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(
        state(cx, &model),
        SyncState::Failed {
            step: SyncStep::Fetch,
            message: "cannot read the upstream status: preflight failed".to_owned()
        }
    );
    assert!(
        stub.state().fetch_requests.is_empty(),
        "no network step may run"
    );
    assert_eq!(model.read_with(cx, |m, _| m.remote_status().cloned()), None);
}

#[gpui::test]
async fn a_cached_missing_upstream_does_not_refuse_once_one_is_configured(cx: &mut TestAppContext) {
    let script = Script::new(vec![
        Some(no_upstream("main")),
        Some(upstream("example.com", 0, 0)),
    ])
    .fetches(vec![Ok(fetched(1))]);
    let (model, stub) = fixture(cx, script);
    refresh(cx, &model).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.remote_status().map(|s| s.has_upstream)),
        Some(false)
    );
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(state(cx, &model), done(1, 0, 0));
    assert_eq!(stub.state().fetch_requests.len(), 1);
}

#[gpui::test]
async fn a_remote_moved_since_the_refresh_is_asked_for_its_own_credentials(
    cx: &mut TestAppContext,
) {
    let store = TestStore::with(&[
        ("old.example.com", HTTPS, credentials("old", "old-token")),
        ("new.example.com", HTTPS, credentials("new", "new-token")),
    ]);
    let script = Script::new(vec![
        Some(upstream("old.example.com", 0, 0)),
        Some(upstream("new.example.com", 0, 0)),
    ])
    .fetches(vec![Ok(fetched(1))]);
    let (model, stub) = with_store(cx, script, &store);
    refresh(cx, &model).await;
    run(cx, &model, SyncModel::fetch).await;
    assert_eq!(stub.fetch_tokens(), [Some("new-token".to_owned())]);
}

#[gpui::test]
async fn titles_follow_the_last_status(cx: &mut TestAppContext) {
    let (model, _) = fixture(cx, Script::new(vec![Some(upstream("example.com", 3, 1))]));
    refresh(cx, &model).await;
    assert_eq!(
        model.read_with(cx, |m, _| push_title(m.remote_status())),
        "Push 3 Commits"
    );
    assert_eq!(
        model.read_with(cx, |m, _| pull_title(m.remote_status())),
        "Pull 1 Commit"
    );
}
