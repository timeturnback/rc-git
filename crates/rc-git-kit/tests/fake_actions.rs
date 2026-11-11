//! The action fake the Changes and Sync model tests stand on: it records each call with its
//! arguments, fails one call on request, refuses to make up an answer, and turns a cancel
//! signalled while a remote step was held into the guard's cancel.

use std::sync::Arc;
use std::thread;

use rc_git_guard::actions::{ActionError, UpstreamStatus};
use rc_git_kit::fake::{ActionCall, ActionKind, CallGate, FakeActions};
use rc_git_kit::guard_bridge::{CancelToken, FailureKind, classify};
use rc_git_kit::repository_actions::{RemoteRequest, RepositoryActions};

fn upstream() -> UpstreamStatus {
    UpstreamStatus {
        branch: "main".to_owned(),
        remote: "origin".to_owned(),
        upstream_ref: "refs/remotes/origin/main".to_owned(),
        url: "https://example.com/r.git".to_owned(),
        host: "example.com".to_owned(),
        ahead: 1,
        behind: 0,
        has_upstream: true,
        published: true,
        uses_ssh: false,
    }
}

#[test]
fn calls_are_recorded_and_a_failure_fires_once() {
    let fake = FakeActions::new();
    fake.state().upstream = Some(upstream());
    fake.state().fail_next.insert(
        ActionKind::UpstreamStatus,
        ActionError::StaleRepository { op: "status" },
    );
    assert_eq!(
        classify(&fake.upstream_status().unwrap_err()),
        FailureKind::Stale
    );
    assert_eq!(fake.upstream_status().unwrap(), upstream());
    fake.create_branch("topic", "main").unwrap();
    let state = fake.state();
    assert_eq!(state.count(ActionKind::UpstreamStatus), 2);
    assert_eq!(
        state.calls.last(),
        Some(&ActionCall::CreateBranch {
            name: "topic".to_owned(),
            revspec: "main".to_owned()
        })
    );
}

#[test]
fn an_answer_nobody_configured_is_an_error_not_a_made_up_value() {
    let fake = FakeActions::new();
    let error = fake.pull_fast_forward().unwrap_err();
    assert!(error.to_string().contains("not configured"), "{error}");
}

#[test]
fn a_cancel_signalled_while_a_fetch_was_held_cancels_it() {
    let fake = FakeActions::new();
    let gate = Arc::new(CallGate::new());
    fake.state()
        .gates
        .insert(ActionKind::Fetch, Arc::clone(&gate));
    let cancel = Arc::new(CancelToken::new());
    let worker = {
        let fake = fake.clone();
        let cancel = Arc::clone(&cancel);
        thread::spawn(move || fake.fetch(&RemoteRequest::default(), &cancel))
    };
    assert!(gate.wait_until_arrived());
    cancel.signal();
    gate.open();
    let error = worker.join().unwrap().unwrap_err();
    assert_eq!(classify(&error), FailureKind::Cancelled);
    assert_eq!(fake.state().count(ActionKind::Fetch), 1);
}
