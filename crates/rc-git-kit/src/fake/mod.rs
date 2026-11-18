//! Test doubles for the two seams: [`FakeRepository`] for
//! [`crate::repository::RepositoryReading`] and [`FakeActions`] for
//! [`crate::repository_actions::RepositoryActions`], plus the [`CallGate`] that orders a race
//! between two loads. Built only with the `test-support` feature, which this crate's own
//! integration tests turn on through a dev-dependency on itself.
//!
//! The fakes are blocking, like the seams. A model runs them through
//! [`crate::blocking::blocking`], which gives each call its own thread, so a gate can hold one
//! call while the test thread drives the models; such a test allows parking on its executor.

mod actions;
mod pull_requests;
mod repository;

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rc_git_core::{Oid, RefEntry, RefKind};

pub use actions::{ActionCall, ActionKind, FakeActions, FakeActionsState};
pub use pull_requests::{
    FakePullRequests, FakePullRequestsState, MergeCall, admin_merge_plan, blocked_detail, linked,
    merge_plan, page, pull_request, pull_request_detail,
};
pub use repository::{FakeRepository, FakeState, commit_summary, text_blob};

/// How long [`CallGate::wait_until_arrived`] waits before it gives up, so a test whose call
/// never comes fails instead of hanging.
pub const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Default)]
struct GateState {
    /// Arrivals no waiter has taken yet.
    arrivals: usize,
    open: bool,
}

/// Holds one blocking call until the test opens it, and tells the test when the call has
/// arrived, so a race between two loads is ordered by the test rather than by sleeps. Only the
/// held call blocks a thread: its own, which [`crate::blocking::blocking`] started for it.
#[derive(Debug, Default)]
pub struct CallGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl CallGate {
    /// A closed gate nobody has reached.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Called by a fake on the worker thread: reports arrival, then blocks until
    /// [`Self::open`].
    pub fn pass(&self) {
        let mut state = self.lock();
        state.arrivals += 1;
        self.changed.notify_all();
        while !state.open {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Blocks the test thread until a call has reached [`Self::pass`], taking that arrival.
    /// `false` when none came within [`ARRIVAL_TIMEOUT`].
    #[must_use]
    pub fn wait_until_arrived(&self) -> bool {
        let state = self.lock();
        let (mut state, timeout) = self
            .changed
            .wait_timeout_while(state, ARRIVAL_TIMEOUT, |state| state.arrivals == 0)
            .unwrap_or_else(PoisonError::into_inner);
        if timeout.timed_out() && state.arrivals == 0 {
            return false;
        }
        state.arrivals -= 1;
        true
    }

    /// Lets the held call return, and every later one pass straight through.
    pub fn open(&self) {
        self.lock().open = true;
        self.changed.notify_all();
    }
}

/// An id whose 20 bytes all equal `seed`, as the Swift tests build them.
///
/// # Panics
/// Never: 20 bytes is always a SHA-1 id.
#[must_use]
pub fn oid(seed: u8) -> Oid {
    #[expect(
        clippy::expect_used,
        reason = "20 bytes is always a SHA-1 id; a test helper, built only for tests"
    )]
    Oid::from_bytes(&[seed; 20]).expect("20 bytes is a SHA-1 id")
}

/// A ref named `refs/heads/<name>` (or the namespace `kind` implies) at `target`.
#[must_use]
pub fn branch(name: &str, target: Oid, kind: RefKind) -> RefEntry {
    let namespace = match kind {
        RefKind::LocalBranch | RefKind::Other => "refs/heads",
        RefKind::RemoteBranch => "refs/remotes",
        RefKind::Tag => "refs/tags",
    };
    RefEntry {
        full_name: format!("{namespace}/{name}"),
        short_name: name.to_owned(),
        kind,
        target,
        exact_name: true,
    }
}
