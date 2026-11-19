//! Merging a pull request from its tab: whether the Merge control shows and why it may not take
//! a click ([`merge_button`], decided here from the guard's own [`merge_blocker`], never in the
//! app), whether the separate admin merge control exists at all ([`admin_merge_button`], from the
//! guard's [`admin_merge_blocker`]), and the merge as one kind of the tab's guarded flow
//! ([`MergeFlow`], [`super::flow`]). A flow is a plain merge or an admin merge from its start
//! ([`MergeKind`]) and stays one; the admin merge's challenge asks for the pull request's number,
//! which the flow hands to the guard as typed. Nothing here retries a merge.

use rc_git_core::Oid;

use super::copy::{merge_attempt_log, merge_outcome_log};
use super::flow::{Flow, Phase, PlanLoad, SendLoad, Stop, TabAction, TabFlow};
use super::link::Linked;
use super::state::DetailState;
use super::{
    AfterLoss, Bypassed, Cancel, Decision, MergeKind, MergeMethod, MergeMethods, MergeOutcome,
    MergePlan, MergeRefusal, PullRequestService, PullRequestStamp, Settled, admin_merge_blocker,
    merge_blocker,
};

/// Why a merge sent nothing, in the terms the tab shows.
pub type MergeStop = Stop<MergeRefusal>;

/// A merge as the user asked for it: the method, and the plain merge or the admin merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MergeIntent {
    /// The method the user chose.
    pub method: MergeMethod,
    /// The plain merge or the admin merge.
    pub kind: MergeKind,
}

/// One merge of one tab.
pub type MergeFlow = Flow<MergeIntent>;

/// Where one merge stands.
pub type MergePhase = Phase<MergeIntent>;

/// The merge to run for a flow, with the user's answer.
pub type MergeLoad = SendLoad<MergeIntent>;

impl Flow<MergeIntent> {
    /// A merge of `kind` of the pull request `stamp` names in `repository` with `method`; see
    /// [`Flow::begin`].
    #[must_use]
    pub fn start(
        generation: u64,
        repository: String,
        stamp: PullRequestStamp,
        method: MergeMethod,
        kind: MergeKind,
    ) -> (Self, PlanLoad<MergeIntent>) {
        Self::begin(generation, repository, stamp, MergeIntent { method, kind })
    }

    /// The method chosen.
    #[must_use]
    pub fn method(&self) -> MergeMethod {
        self.ask().method
    }

    /// The plain merge or the admin merge.
    #[must_use]
    pub fn kind(&self) -> MergeKind {
        self.ask().kind
    }

    /// What an admin merge skips, as its plan's read reported it; `None` before the plan
    /// landed, and always for the plain merge.
    #[must_use]
    pub fn bypassed(&self) -> Option<&Bypassed> {
        self.planned_as()?.bypassed.as_ref()
    }
}

impl TabAction for MergeIntent {
    type Plan = MergePlan;
    type Outcome = MergeOutcome;
    type Refusal = MergeRefusal;
    type End = MergeEnd;

    fn decision(plan: &MergePlan) -> &Decision {
        &plan.decision
    }

    fn end(answer: Result<MergeOutcome, MergeStop>) -> MergeEnd {
        match answer {
            Ok(MergeOutcome::Merged {
                merge_commit,
                base,
                settled,
            }) => MergeEnd::Merged {
                merge_commit,
                base,
                settled,
            },
            Ok(MergeOutcome::MergedIntoOtherBase {
                confirmed,
                actual,
                merge_commit,
                settled,
            }) => MergeEnd::MergedIntoOtherBase {
                confirmed,
                actual,
                merge_commit,
                settled,
            },
            Ok(MergeOutcome::Refused { reason }) => MergeEnd::Refused { reason },
            Ok(MergeOutcome::Unknown { problem, after }) => MergeEnd::Unknown { problem, after },
            Err(stop) => MergeEnd::NotSent(stop),
        }
    }

    fn was_sent(end: &MergeEnd) -> bool {
        end.was_sent()
    }

    fn ended_changed(end: &MergeEnd) -> bool {
        matches!(end, MergeEnd::NotSent(stop) if stop.is_changed())
    }

    fn changed(&self, number: u64, what: &str) -> MergeRefusal {
        MergeRefusal::Changed {
            pull_request: format!("#{number}"),
            what: what.to_owned(),
        }
    }

    fn plan(
        &self,
        service: &dyn PullRequestService,
        linked: &Linked,
        stamp: &PullRequestStamp,
        cancel: &Cancel,
    ) -> Result<MergePlan, MergeStop> {
        let ask = PlanAsk {
            stamp,
            method: self.method,
            kind: self.kind,
        };
        service.plan_merge(linked, ask, cancel)
    }

    fn send(
        service: &dyn PullRequestService,
        linked: &Linked,
        load: &MergeLoad,
        on_send: &(dyn Fn() + Sync),
        cancel: &Cancel,
    ) -> Result<MergeOutcome, MergeStop> {
        let ask = MergeAsk {
            stamp: &load.stamp,
            method: load.ask.method,
            kind: load.ask.kind,
            decision: Some(&load.decision),
            typed: &load.typed,
            on_send,
        };
        service.merge(linked, ask, cancel)
    }

    fn attempt_log(flow: &MergeFlow) -> String {
        merge_attempt_log(flow)
    }

    fn outcome_log(flow: &MergeFlow, end: &MergeEnd) -> String {
        merge_outcome_log(flow, end)
    }

    fn wrap(flow: MergeFlow) -> TabFlow {
        TabFlow::Merge(flow)
    }

    fn of(tab: &TabFlow) -> Option<&MergeFlow> {
        match tab {
            TabFlow::Merge(flow) => Some(flow),
            TabFlow::AutoMerge(_) | TabFlow::Rerun(_) | TabFlow::DeleteBranch(_) => None,
        }
    }

    fn of_mut(tab: &mut TabFlow) -> Option<&mut MergeFlow> {
        match tab {
            TabFlow::Merge(flow) => Some(flow),
            TabFlow::AutoMerge(_) | TabFlow::Rerun(_) | TabFlow::DeleteBranch(_) => None,
        }
    }
}

/// What the merge service is asked to plan: the pull request the user saw, the method, and the
/// plain merge or the admin merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanAsk<'a> {
    /// What the user saw.
    pub stamp: &'a PullRequestStamp,
    /// The method the user chose.
    pub method: MergeMethod,
    /// The plain merge or the admin merge.
    pub kind: MergeKind,
}

/// What the merge service is asked to merge, with the user's answer.
#[derive(Clone, Copy)]
pub struct MergeAsk<'a> {
    /// What the user saw.
    pub stamp: &'a PullRequestStamp,
    /// The method the user chose.
    pub method: MergeMethod,
    /// The plain merge or the admin merge, as the flow started.
    pub kind: MergeKind,
    /// The decision the challenge showed; `None` when the user did not confirm.
    pub decision: Option<&'a Decision>,
    /// What the user typed in the challenge, untouched: the guard verifies it.
    pub typed: &'a str,
    /// Called once, just before the request leaves.
    pub on_send: &'a (dyn Fn() + Sync),
}

impl std::fmt::Debug for MergeAsk<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergeAsk")
            .field("stamp", self.stamp)
            .field("method", &self.method)
            .field("kind", &self.kind)
            .field("decision", &self.decision)
            .finish_non_exhaustive()
    }
}

/// The methods `allowed` offers, in the order the control lists them: squash, then a merge
/// commit, then rebase.
#[must_use]
pub fn allowed_methods(allowed: &MergeMethods) -> Vec<MergeMethod> {
    let mut methods = Vec::new();
    for method in [MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase] {
        if allowed.allows(method) {
            methods.push(method);
        }
    }
    methods
}

/// What the Merge control shows for a tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeButton {
    /// No control: nothing current is on screen (the first read, or a failed refresh), so
    /// there is no stamp a merge could stand for.
    Hidden,
    /// Shown but not clickable, with why.
    Disabled {
        /// The sentence under the control.
        reason: String,
    },
    /// Clickable, with the methods to choose from, the default first.
    Enabled {
        /// The repository's allowed methods, in [`allowed_methods`] order.
        methods: Vec<MergeMethod>,
    },
}

/// The Merge control for `tab`, signed in when `linked`. Uses the guard's [`merge_blocker`], so
/// the control and the merge itself refuse alike; the guard decides again on its own read.
#[must_use]
pub fn merge_button(tab: &DetailState, linked: bool) -> MergeButton {
    let (Some(_), Some(detail)) = (tab.stamp(), tab.detail()) else {
        return MergeButton::Hidden;
    };
    if !linked {
        return MergeButton::Disabled {
            reason: super::copy::MERGE_SIGNED_OUT.to_owned(),
        };
    }
    let methods = allowed_methods(&detail.merge_methods);
    let Some(first) = methods.first() else {
        return MergeButton::Disabled {
            reason: super::copy::MERGE_NO_METHOD.to_owned(),
        };
    };
    match merge_blocker(detail, *first) {
        Some(blocker) => MergeButton::Disabled {
            reason: super::copy::merge_blocked(&blocker),
        },
        None => MergeButton::Enabled { methods },
    }
}

/// The admin merge control of a tab: there or not, never greyed out with a reason. It exists only
/// when the plain merge is blocked and GitHub says the account may merge past that protection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminMergeButton {
    /// No control: nothing current is on screen, the account is signed out, or the guard finds
    /// something in the way of an admin merge (nothing to go past included).
    Hidden,
    /// Clickable, with the methods to choose from, the default first.
    Shown {
        /// The repository's allowed methods, in [`allowed_methods`] order.
        methods: Vec<MergeMethod>,
    },
}

/// The admin merge control for `tab`, signed in when `linked`. Uses the guard's
/// [`admin_merge_blocker`], so the control and the admin merge itself refuse alike; the guard
/// decides again on its own read, and its challenge asks for the number.
#[must_use]
pub fn admin_merge_button(tab: &DetailState, linked: bool) -> AdminMergeButton {
    let (Some(_), Some(detail)) = (tab.stamp(), tab.detail()) else {
        return AdminMergeButton::Hidden;
    };
    let methods = allowed_methods(&detail.merge_methods);
    let Some(first) = methods.first() else {
        return AdminMergeButton::Hidden;
    };
    if !linked || admin_merge_blocker(detail, *first).is_some() {
        return AdminMergeButton::Hidden;
    }
    AdminMergeButton::Shown { methods }
}

/// How a merge ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeEnd {
    /// Nothing was sent.
    NotSent(MergeStop),
    /// GitHub merged it, into the confirmed base when it named one.
    Merged {
        /// The merge commit, when GitHub named one.
        merge_commit: Option<Oid>,
        /// The base GitHub says it merged into, always the confirmed one; `None` when GitHub
        /// did not say, so nothing may name a base as fact.
        base: Option<String>,
        /// How rc-git knows.
        settled: Settled,
    },
    /// GitHub merged it at the confirmed head into another base than the confirmed one: it was
    /// retargeted after the last read. Shown as a warning, never as a plain success.
    MergedIntoOtherBase {
        /// The base the user confirmed.
        confirmed: String,
        /// The base GitHub merged it into.
        actual: String,
        /// The merge commit, when GitHub named one.
        merge_commit: Option<Oid>,
        /// How rc-git knows.
        settled: Settled,
    },
    /// GitHub refused it; nothing merged.
    Refused {
        /// GitHub's reason.
        reason: String,
    },
    /// The request left and no usable answer came back.
    Unknown {
        /// What went wrong with the answer.
        problem: String,
        /// What reading the pull request again found.
        after: AfterLoss,
    },
}

impl MergeEnd {
    /// Whether the request may have reached GitHub, so the tab and the list read again.
    #[must_use]
    pub fn was_sent(&self) -> bool {
        !matches!(self, Self::NotSent(_))
    }
}
