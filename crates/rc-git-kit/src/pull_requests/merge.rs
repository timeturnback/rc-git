//! Merging a pull request from its tab: whether the Merge control shows and why it may not take
//! a click ([`merge_button`], decided here from the guard's own [`merge_blocker`], never in the
//! app), whether the separate admin merge control exists at all ([`admin_merge_button`], from the
//! guard's [`admin_merge_blocker`]), and one merge's flow as a state machine free of GPUI
//! ([`MergeFlow`]): planning (the guard reads the pull request again and the policy words the
//! challenge), confirming (the challenge is up), merging (Cancel works until the request is sent,
//! never after), and done. A flow is a plain merge or an admin merge from its start
//! ([`MergeKind`]) and stays one; the admin merge's challenge asks for the pull request's number,
//! which the flow hands to the guard as typed.
//!
//! Every step's answer is taken only under the generation it was asked for, and a flow is tied
//! to the stamp it started from: a read of the tab that shows another head, base or state while
//! the flow plans or confirms ends it before anything is sent. Nothing here retries a merge.

use rc_git_core::Oid;

use super::state::DetailState;
use super::{
    AfterLoss, Bypassed, Decision, MergeKind, MergeMethod, MergeMethods, MergeOutcome, MergePlan,
    MergeRefusal, PullRequestStamp, Settled, admin_merge_blocker, merge_blocker,
};

/// Why a merge sent nothing, in the terms the tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeStop {
    /// The user cancelled before the request left.
    Cancelled,
    /// The guard refused before sending: changed since the stamp, blocked, not confirmed, a
    /// failed read, the policy.
    Refused(MergeRefusal),
    /// The account has no usable token.
    SignedOut {
        /// The account (`login@host`), or the host that refused the token.
        account: String,
    },
    /// Anything else before the request: the token could not be read or renewed.
    Failed(String),
}

impl MergeStop {
    /// Whether the refusal says the pull request changed since the stamp, so the tab should
    /// read it again.
    #[must_use]
    pub fn is_changed(&self) -> bool {
        matches!(self, Self::Refused(MergeRefusal::Changed { .. }))
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

/// Where one merge stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergePhase {
    /// The guard reads the pull request again and the policy words the challenge.
    Planning,
    /// The challenge is up.
    Confirming(Box<MergePlan>),
    /// The merge runs. Until `sent`, Cancel stops it; after, nothing can.
    Merging {
        /// Whether the request left.
        sent: bool,
        /// Whether a cancel was asked for and the answer is awaited.
        cancelling: bool,
    },
    /// Finished.
    Done(MergeEnd),
}

/// The plan read to run for a flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLoad {
    /// Answer [`MergeFlow::planned`] with it.
    pub generation: u64,
    /// What the user saw.
    pub stamp: PullRequestStamp,
    /// The method chosen.
    pub method: MergeMethod,
    /// The plain merge or the admin merge.
    pub kind: MergeKind,
}

/// The merge to run for a flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeLoad {
    /// Answer [`MergeFlow::sent`] and [`MergeFlow::finished`] with it.
    pub generation: u64,
    /// What the user saw.
    pub stamp: PullRequestStamp,
    /// The method chosen.
    pub method: MergeMethod,
    /// The plain merge or the admin merge.
    pub kind: MergeKind,
    /// The decision the user answered.
    pub decision: Decision,
    /// What the user typed, untouched.
    pub typed: String,
}

/// What a cancel did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelAnswer {
    /// The flow ended before anything was sent: drop it.
    Ended,
    /// The merge runs and its request has not left: raise its cancel flag.
    Signal,
    /// The request left: nothing can stop it.
    TooLate,
    /// The flow is done: nothing to cancel.
    Nothing,
}

/// One merge of one tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeFlow {
    repository: String,
    number: u64,
    generation: u64,
    stamp: PullRequestStamp,
    method: MergeMethod,
    kind: MergeKind,
    bypassed: Option<Bypassed>,
    phase: MergePhase,
}

impl MergeFlow {
    /// A merge of `kind` of the pull request `stamp` names in `repository` (`owner/name@host`,
    /// as the log and the notes name it for as long as the flow lives, whatever the window
    /// links to later) with `method`, under `generation` (unique among the window's merges),
    /// and the plan read to run.
    #[must_use]
    pub fn start(
        generation: u64,
        repository: String,
        stamp: PullRequestStamp,
        method: MergeMethod,
        kind: MergeKind,
    ) -> (Self, PlanLoad) {
        let load = PlanLoad {
            generation,
            stamp: stamp.clone(),
            method,
            kind,
        };
        let flow = Self {
            repository,
            number: stamp.number,
            generation,
            stamp,
            method,
            kind,
            bypassed: None,
            phase: MergePhase::Planning,
        };
        (flow, load)
    }

    /// The repository it merges in, `owner/name@host`, as it was when the flow started.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// The pull request's number.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.number
    }

    /// The generation every answer must carry.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The stamp it started from.
    #[must_use]
    pub fn stamp(&self) -> &PullRequestStamp {
        &self.stamp
    }

    /// The method chosen.
    #[must_use]
    pub fn method(&self) -> MergeMethod {
        self.method
    }

    /// The plain merge or the admin merge.
    #[must_use]
    pub fn kind(&self) -> MergeKind {
        self.kind
    }

    /// What an admin merge skips, as its plan's read reported it; `None` before the plan
    /// landed, and always for the plain merge.
    #[must_use]
    pub fn bypassed(&self) -> Option<&Bypassed> {
        self.bypassed.as_ref()
    }

    /// Where it stands.
    #[must_use]
    pub fn phase(&self) -> &MergePhase {
        &self.phase
    }

    /// The challenge's plan, while it is up.
    #[must_use]
    pub fn plan(&self) -> Option<&MergePlan> {
        match &self.phase {
            MergePhase::Confirming(plan) => Some(plan),
            MergePhase::Planning | MergePhase::Merging { .. } | MergePhase::Done(_) => None,
        }
    }

    /// Takes the plan read of `generation`; any other is dropped. Answers whether it landed.
    pub fn planned(&mut self, generation: u64, answer: Result<MergePlan, MergeStop>) -> bool {
        if generation != self.generation || self.phase != MergePhase::Planning {
            return false;
        }
        self.phase = match answer {
            Ok(plan) => {
                self.bypassed.clone_from(&plan.bypassed);
                MergePhase::Confirming(Box::new(plan))
            }
            Err(stop) => MergePhase::Done(MergeEnd::NotSent(stop)),
        };
        true
    }

    /// The user confirmed with `typed`: the merge to run, only while the challenge is up. The
    /// text goes to the guard as typed; the guard alone verifies it.
    pub fn confirm(&mut self, typed: String) -> Option<MergeLoad> {
        let MergePhase::Confirming(plan) = &self.phase else {
            return None;
        };
        let load = MergeLoad {
            generation: self.generation,
            stamp: self.stamp.clone(),
            method: self.method,
            kind: self.kind,
            decision: plan.decision.clone(),
            typed,
        };
        self.phase = MergePhase::Merging {
            sent: false,
            cancelling: false,
        };
        Some(load)
    }

    /// The request of `generation` left.
    pub fn sent(&mut self, generation: u64) {
        if generation != self.generation {
            return;
        }
        if let MergePhase::Merging { sent, .. } = &mut self.phase {
            *sent = true;
        }
    }

    /// Takes the merge's answer of `generation`; any other is dropped. Answers whether it
    /// landed.
    pub fn finished(&mut self, generation: u64, answer: Result<MergeOutcome, MergeStop>) -> bool {
        if generation != self.generation || !matches!(self.phase, MergePhase::Merging { .. }) {
            return false;
        }
        self.phase = MergePhase::Done(match answer {
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
        });
        true
    }

    /// The user asked to cancel.
    pub fn cancel(&mut self) -> CancelAnswer {
        match &mut self.phase {
            MergePhase::Planning | MergePhase::Confirming(_) => {
                self.phase = MergePhase::Done(MergeEnd::NotSent(MergeStop::Cancelled));
                CancelAnswer::Ended
            }
            MergePhase::Merging { sent: true, .. } => CancelAnswer::TooLate,
            MergePhase::Merging { cancelling, .. } => {
                *cancelling = true;
                CancelAnswer::Signal
            }
            MergePhase::Done(_) => CancelAnswer::Nothing,
        }
    }

    /// The tab read `now` (its stamp, or `None` when the read failed): a flow that plans or
    /// confirms ends when it no longer matches, since the challenge would stand for something
    /// the user did not see. A running merge is the guard's to judge. Answers whether it ended.
    pub fn follow(&mut self, now: Option<&PullRequestStamp>) -> bool {
        if !matches!(self.phase, MergePhase::Planning | MergePhase::Confirming(_)) {
            return false;
        }
        if now == Some(&self.stamp) {
            return false;
        }
        let what = match now {
            Some(_) => "the tab read it again and it is not what the challenge was for",
            None => "the tab's read of it failed, so the screen no longer shows it as it is",
        };
        self.phase = MergePhase::Done(MergeEnd::NotSent(MergeStop::Refused(
            MergeRefusal::Changed {
                pull_request: format!("#{}", self.number),
                what: what.to_owned(),
            },
        )));
        true
    }
}
