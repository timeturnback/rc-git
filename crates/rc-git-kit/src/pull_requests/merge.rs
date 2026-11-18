//! Merging a pull request from its tab: whether the Merge control shows and why it may not take
//! a click ([`merge_button`], decided here from the guard's own [`merge_blocker`], never in the
//! app), and one merge's flow as a state machine free of GPUI ([`MergeFlow`]): planning (the
//! guard reads the pull request again and the policy words the challenge), confirming (the
//! challenge is up), merging (Cancel works until the request is sent, never after), and done.
//!
//! Every step's answer is taken only under the generation it was asked for, and a flow is tied
//! to the stamp it started from: a read of the tab that shows another head, base or state while
//! the flow plans or confirms ends it before anything is sent. Nothing here retries a merge.

use rc_git_core::Oid;

use super::state::DetailState;
use super::{
    AfterLoss, Decision, MergeMethod, MergeMethods, MergeOutcome, MergePlan, MergeRefusal,
    PullRequestStamp, Settled, merge_blocker,
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

/// What the merge service is asked to merge, with the user's answer.
#[derive(Clone, Copy)]
pub struct MergeAsk<'a> {
    /// What the user saw.
    pub stamp: &'a PullRequestStamp,
    /// The method the user chose.
    pub method: MergeMethod,
    /// The decision the challenge showed; `None` when the user did not confirm.
    pub decision: Option<&'a Decision>,
    /// What the user typed in the challenge; empty for a plain confirm.
    pub typed: &'a str,
    /// Called once, just before the request leaves.
    pub on_send: &'a (dyn Fn() + Sync),
}

impl std::fmt::Debug for MergeAsk<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergeAsk")
            .field("stamp", self.stamp)
            .field("method", &self.method)
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

/// How a merge ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeEnd {
    /// Nothing was sent.
    NotSent(MergeStop),
    /// GitHub merged it.
    Merged {
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
    /// The decision the user answered.
    pub decision: Decision,
    /// What the user typed.
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
    number: u64,
    generation: u64,
    stamp: PullRequestStamp,
    method: MergeMethod,
    phase: MergePhase,
}

impl MergeFlow {
    /// A merge of the pull request `stamp` names with `method`, under `generation` (unique
    /// among the window's merges), and the plan read to run.
    #[must_use]
    pub fn start(
        generation: u64,
        stamp: PullRequestStamp,
        method: MergeMethod,
    ) -> (Self, PlanLoad) {
        let load = PlanLoad {
            generation,
            stamp: stamp.clone(),
            method,
        };
        let flow = Self {
            number: stamp.number,
            generation,
            stamp,
            method,
            phase: MergePhase::Planning,
        };
        (flow, load)
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
            Ok(plan) => MergePhase::Confirming(Box::new(plan)),
            Err(stop) => MergePhase::Done(MergeEnd::NotSent(stop)),
        };
        true
    }

    /// The user confirmed with `typed`: the merge to run, only while the challenge is up.
    pub fn confirm(&mut self, typed: String) -> Option<MergeLoad> {
        let MergePhase::Confirming(plan) = &self.phase else {
            return None;
        };
        let load = MergeLoad {
            generation: self.generation,
            stamp: self.stamp.clone(),
            method: self.method,
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
                settled,
            }) => MergeEnd::Merged {
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
