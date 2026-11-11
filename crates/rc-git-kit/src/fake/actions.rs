//! An in-memory [`RepositoryActions`] for model tests: it records every call with its
//! arguments, answers with what the test configured, fails a method once on request, and can
//! hold a call at a [`CallGate`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rc_git_core::{DefaultBranch, Oid, StashEntry};
use rc_git_guard::actions::{
    ActionError, ActionResult, Author, CheckoutOutcome, CommitRequest, Committed, Deleted,
    DiscardOutcome, FetchOutcome, FileSelection, HunkStamp, Identity, PullOutcome, PushOutcome,
    StashRequest, StashSaved, UpstreamStatus,
};

use super::CallGate;
use crate::guard_bridge::{CancelToken, GuardDecision};
use crate::repository_actions::{RemoteRequest, RepositoryActions};

/// Which action a configured failure, gate or answer applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    missing_docs,
    reason = "each variant names the trait method of the same name"
)]
pub enum ActionKind {
    PlanBranchDelete,
    CreateBranch,
    CreateTag,
    DeleteBranch,
    DeleteTag,
    ConfiguredIdentity,
    Commit,
    StashPush,
    StashApply,
    StashPop,
    StashDrop,
    DiscardFiles,
    DiscardHunks,
    UpstreamStatus,
    DefaultBranch,
    Fetch,
    PullFastForward,
    Checkout,
    Push,
}

/// One recorded call and the arguments it carried.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    missing_docs,
    reason = "each variant mirrors the trait method's parameters"
)]
pub enum ActionCall {
    PlanBranchDelete {
        name: String,
    },
    CreateBranch {
        name: String,
        revspec: String,
    },
    CreateTag {
        name: String,
        revspec: String,
    },
    DeleteBranch {
        name: String,
        input: Option<String>,
    },
    DeleteTag {
        name: String,
        expected: Oid,
        input: Option<String>,
    },
    ConfiguredIdentity,
    Commit {
        message: String,
        author: Author,
        expected_head: Option<Oid>,
        selections: Vec<FileSelection>,
        input: Option<String>,
    },
    StashPush {
        paths: Vec<String>,
        message: Option<String>,
        author: Author,
        input: Option<String>,
    },
    StashApply {
        entry: StashEntry,
        input: Option<String>,
    },
    StashPop {
        entry: StashEntry,
        input: Option<String>,
    },
    StashDrop {
        entry: StashEntry,
        input: Option<String>,
    },
    DiscardFiles {
        paths: Vec<String>,
        input: Option<String>,
    },
    DiscardHunks {
        path: String,
        stamps: Vec<HunkStamp>,
        input: Option<String>,
    },
    UpstreamStatus,
    DefaultBranch,
    Fetch {
        remote: RemoteRequest,
    },
    PullFastForward,
    Checkout {
        branch: String,
    },
    Push {
        remote: RemoteRequest,
    },
}

/// What the fake answers and what it recorded. An answer left `None` fails the call with a
/// message saying the fake was not configured, so a test never reads a made-up value.
#[derive(Debug, Default)]
pub struct FakeActionsState {
    /// Every call, in order.
    pub calls: Vec<ActionCall>,
    /// Per action, an error the next call returns, then clears.
    pub fail_next: HashMap<ActionKind, ActionError>,
    /// Per action, holds the next call, then clears itself. A remote step whose token was
    /// signalled while it was held returns [`ActionError::Cancelled`].
    pub gates: HashMap<ActionKind, Arc<CallGate>>,
    /// What `plan_branch_delete` answers.
    pub branch_delete_plan: Option<GuardDecision>,
    /// What the deletes and the stash pop and drop answer.
    pub deleted: Option<Deleted>,
    /// What `configured_identity` answers.
    pub identity: Option<Identity>,
    /// What `commit` answers.
    pub committed: Option<Committed>,
    /// What `stash_push` answers.
    pub stash_saved: Option<StashSaved>,
    /// What `upstream_status` answers.
    pub upstream: Option<UpstreamStatus>,
    /// What `default_branch` answers.
    pub default_branch: Option<DefaultBranch>,
    /// What `fetch` answers.
    pub fetched: Option<FetchOutcome>,
    /// What `pull_fast_forward` answers.
    pub pulled: Option<PullOutcome>,
    /// What `checkout` answers.
    pub checked_out: Option<CheckoutOutcome>,
    /// What `push` answers.
    pub pushed: Option<PushOutcome>,
}

impl FakeActionsState {
    /// How many calls of `kind` were recorded.
    #[must_use]
    pub fn count(&self, kind: ActionKind) -> usize {
        self.calls.iter().filter(|call| call.kind() == kind).count()
    }
}

impl ActionCall {
    /// The action this call was.
    #[must_use]
    pub fn kind(&self) -> ActionKind {
        match self {
            Self::PlanBranchDelete { .. } => ActionKind::PlanBranchDelete,
            Self::CreateBranch { .. } => ActionKind::CreateBranch,
            Self::CreateTag { .. } => ActionKind::CreateTag,
            Self::DeleteBranch { .. } => ActionKind::DeleteBranch,
            Self::DeleteTag { .. } => ActionKind::DeleteTag,
            Self::ConfiguredIdentity => ActionKind::ConfiguredIdentity,
            Self::Commit { .. } => ActionKind::Commit,
            Self::StashPush { .. } => ActionKind::StashPush,
            Self::StashApply { .. } => ActionKind::StashApply,
            Self::StashPop { .. } => ActionKind::StashPop,
            Self::StashDrop { .. } => ActionKind::StashDrop,
            Self::DiscardFiles { .. } => ActionKind::DiscardFiles,
            Self::DiscardHunks { .. } => ActionKind::DiscardHunks,
            Self::UpstreamStatus => ActionKind::UpstreamStatus,
            Self::DefaultBranch => ActionKind::DefaultBranch,
            Self::Fetch { .. } => ActionKind::Fetch,
            Self::PullFastForward => ActionKind::PullFastForward,
            Self::Checkout { .. } => ActionKind::Checkout,
            Self::Push { .. } => ActionKind::Push,
        }
    }
}

/// The fake. Cloning shares the state.
#[derive(Debug, Clone, Default)]
pub struct FakeActions {
    state: Arc<Mutex<FakeActionsState>>,
}

fn owned(input: Option<&str>) -> Option<String> {
    input.map(str::to_owned)
}

fn not_configured(kind: ActionKind) -> ActionError {
    ActionError::Git {
        op: "fake action",
        name: format!("{kind:?}"),
        source: "the fake was not configured with an answer".into(),
    }
}

impl FakeActions {
    /// A fake with no answers configured.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The answers and the call log, locked.
    pub fn state(&self) -> MutexGuard<'_, FakeActionsState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records `call`, passes its gate, then returns its configured failure, if any.
    fn enter(&self, call: ActionCall) -> ActionResult<()> {
        let kind = call.kind();
        let gate = {
            let mut state = self.state();
            state.calls.push(call);
            state.gates.remove(&kind)
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        match self.state().fail_next.remove(&kind) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// As [`Self::enter`], then a cancel if `cancel` was signalled meanwhile.
    fn enter_remote(
        &self,
        call: ActionCall,
        op: &'static str,
        cancel: &CancelToken,
    ) -> ActionResult<()> {
        self.enter(call)?;
        if cancel.is_signalled() {
            return Err(ActionError::Cancelled { op });
        }
        Ok(())
    }

    /// The answer `pick` reads from the state, or the not-configured error.
    fn answer<T>(
        &self,
        kind: ActionKind,
        pick: impl FnOnce(&FakeActionsState) -> Option<T>,
    ) -> ActionResult<T> {
        pick(&self.state()).ok_or_else(|| not_configured(kind))
    }
}

impl RepositoryActions for FakeActions {
    fn plan_branch_delete(&self, name: &str) -> ActionResult<GuardDecision> {
        self.enter(ActionCall::PlanBranchDelete {
            name: name.to_owned(),
        })?;
        self.answer(ActionKind::PlanBranchDelete, |state| {
            state.branch_delete_plan.clone()
        })
    }

    fn create_branch(&self, name: &str, revspec: &str) -> ActionResult<()> {
        self.enter(ActionCall::CreateBranch {
            name: name.to_owned(),
            revspec: revspec.to_owned(),
        })
    }

    fn create_tag(&self, name: &str, revspec: &str) -> ActionResult<()> {
        self.enter(ActionCall::CreateTag {
            name: name.to_owned(),
            revspec: revspec.to_owned(),
        })
    }

    fn delete_branch(&self, name: &str, input: Option<&str>) -> ActionResult<Deleted> {
        self.enter(ActionCall::DeleteBranch {
            name: name.to_owned(),
            input: owned(input),
        })?;
        self.answer(ActionKind::DeleteBranch, |state| state.deleted.clone())
    }

    fn delete_tag(&self, name: &str, expected: Oid, input: Option<&str>) -> ActionResult<Deleted> {
        self.enter(ActionCall::DeleteTag {
            name: name.to_owned(),
            expected,
            input: owned(input),
        })?;
        self.answer(ActionKind::DeleteTag, |state| state.deleted.clone())
    }

    fn configured_identity(&self) -> ActionResult<Option<Identity>> {
        self.enter(ActionCall::ConfiguredIdentity)?;
        Ok(self.state().identity.clone())
    }

    fn commit(&self, request: CommitRequest<'_>, input: Option<&str>) -> ActionResult<Committed> {
        self.enter(ActionCall::Commit {
            message: request.message.to_owned(),
            author: request.author,
            expected_head: request.expected_head,
            selections: request.selections.to_vec(),
            input: owned(input),
        })?;
        self.answer(ActionKind::Commit, |state| state.committed.clone())
    }

    fn stash_push(
        &self,
        request: StashRequest<'_>,
        input: Option<&str>,
    ) -> ActionResult<StashSaved> {
        self.enter(ActionCall::StashPush {
            paths: request.paths.to_vec(),
            message: request.message.map(str::to_owned),
            author: request.author,
            input: owned(input),
        })?;
        self.answer(ActionKind::StashPush, |state| state.stash_saved.clone())
    }

    fn stash_apply(&self, entry: &StashEntry, input: Option<&str>) -> ActionResult<()> {
        self.enter(ActionCall::StashApply {
            entry: entry.clone(),
            input: owned(input),
        })
    }

    fn stash_pop(&self, entry: &StashEntry, input: Option<&str>) -> ActionResult<Deleted> {
        self.enter(ActionCall::StashPop {
            entry: entry.clone(),
            input: owned(input),
        })?;
        self.answer(ActionKind::StashPop, |state| state.deleted.clone())
    }

    fn stash_drop(&self, entry: &StashEntry, input: Option<&str>) -> ActionResult<Deleted> {
        self.enter(ActionCall::StashDrop {
            entry: entry.clone(),
            input: owned(input),
        })?;
        self.answer(ActionKind::StashDrop, |state| state.deleted.clone())
    }

    fn discard_files(
        &self,
        paths: &[String],
        input: Option<&str>,
    ) -> ActionResult<Vec<DiscardOutcome>> {
        self.enter(ActionCall::DiscardFiles {
            paths: paths.to_vec(),
            input: owned(input),
        })?;
        Ok(Vec::new())
    }

    fn discard_hunks(
        &self,
        path: &str,
        stamps: &[HunkStamp],
        input: Option<&str>,
    ) -> ActionResult<()> {
        self.enter(ActionCall::DiscardHunks {
            path: path.to_owned(),
            stamps: stamps.to_vec(),
            input: owned(input),
        })
    }

    fn upstream_status(&self) -> ActionResult<UpstreamStatus> {
        self.enter(ActionCall::UpstreamStatus)?;
        self.answer(ActionKind::UpstreamStatus, |state| state.upstream.clone())
    }

    fn default_branch(&self) -> ActionResult<Option<DefaultBranch>> {
        self.enter(ActionCall::DefaultBranch)?;
        Ok(self.state().default_branch.clone())
    }

    fn fetch(&self, remote: &RemoteRequest, cancel: &CancelToken) -> ActionResult<FetchOutcome> {
        let call = ActionCall::Fetch {
            remote: remote.clone(),
        };
        self.enter_remote(call, "fetch", cancel)?;
        self.answer(ActionKind::Fetch, |state| state.fetched.clone())
    }

    fn pull_fast_forward(&self) -> ActionResult<PullOutcome> {
        self.enter(ActionCall::PullFastForward)?;
        self.answer(ActionKind::PullFastForward, |state| state.pulled.clone())
    }

    fn checkout(&self, branch: &str) -> ActionResult<CheckoutOutcome> {
        self.enter(ActionCall::Checkout {
            branch: branch.to_owned(),
        })?;
        self.answer(ActionKind::Checkout, |state| state.checked_out.clone())
    }

    fn push(&self, remote: &RemoteRequest, cancel: &CancelToken) -> ActionResult<PushOutcome> {
        let call = ActionCall::Push {
            remote: remote.clone(),
        };
        self.enter_remote(call, "push", cancel)?;
        self.answer(ActionKind::Push, |state| state.pushed.clone())
    }
}
