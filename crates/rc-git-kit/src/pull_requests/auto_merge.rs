//! Auto-merge from a tab: what the auto-merge control shows ([`auto_merge_control`], decided here
//! from the guard's own blockers, never in the app), and turning it on or off as one kind of the
//! tab's guarded flow ([`AutoMergeFlow`], [`super::flow`]).

use super::copy::{auto_merge_attempt_log, auto_merge_outcome_log};
use super::flow::{ActionAsk, ActionStop, Ended, Flow, SendLoad, Stop, TabAction, TabFlow};
use super::link::Linked;
use super::merge::allowed_methods;
use super::state::DetailState;
use super::{
    ActionPlan, ActionRefusal, AutoMergeChange, AutoMergeOutcome, Cancel, Decision, MergeMethod,
    PullRequestAction, PullRequestService, PullRequestStamp, disable_auto_merge_blocker,
    enable_auto_merge_blocker,
};

/// An auto-merge change as the user asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AutoMergeIntent(pub AutoMergeChange);

/// One auto-merge change of one tab.
pub type AutoMergeFlow = Flow<AutoMergeIntent>;

/// How an auto-merge change ended.
pub type AutoMergeEnd = Ended<AutoMergeOutcome, ActionRefusal>;

/// What the auto-merge control shows for a tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoMergeControl {
    /// No control: nothing current is on screen, the account is signed out, or the guard finds
    /// something in the way of turning it on (it can merge now included).
    Hidden,
    /// Auto-merge is off and may be turned on, with any of these methods, the default first.
    Offer {
        /// The methods the guard allows it with, in [`allowed_methods`] order.
        methods: Vec<MergeMethod>,
    },
    /// Auto-merge is on.
    On {
        /// The method it will merge with, when the host named one this build knows.
        method: Option<MergeMethod>,
        /// Who turned it on, single-line.
        enabled_by: Option<String>,
        /// Whether it may be turned off from here: signed in, and the guard finds nothing in
        /// the way.
        can_turn_off: bool,
    },
}

/// The auto-merge control for `tab`, signed in when `linked`. Uses the guard's
/// [`enable_auto_merge_blocker`] and [`disable_auto_merge_blocker`], so the control and the
/// change itself refuse alike; the guard decides again on its own read.
#[must_use]
pub fn auto_merge_control(tab: &DetailState, linked: bool) -> AutoMergeControl {
    let (Some(_), Some(detail)) = (tab.stamp(), tab.detail()) else {
        return AutoMergeControl::Hidden;
    };
    if let Some(auto) = &detail.auto_merge {
        return AutoMergeControl::On {
            method: auto.method,
            enabled_by: auto.enabled_by.clone(),
            can_turn_off: linked && disable_auto_merge_blocker(detail).is_none(),
        };
    }
    if !linked {
        return AutoMergeControl::Hidden;
    }
    let mut methods = Vec::new();
    for method in allowed_methods(&detail.merge_methods) {
        if enable_auto_merge_blocker(detail, method).is_none() {
            methods.push(method);
        }
    }
    if methods.is_empty() {
        return AutoMergeControl::Hidden;
    }
    AutoMergeControl::Offer { methods }
}

impl TabAction for AutoMergeIntent {
    type Plan = ActionPlan;
    type Outcome = AutoMergeOutcome;
    type Refusal = ActionRefusal;
    type End = AutoMergeEnd;

    fn decision(plan: &ActionPlan) -> &Decision {
        &plan.decision
    }

    fn end(answer: Result<AutoMergeOutcome, ActionStop>) -> AutoMergeEnd {
        match answer {
            Ok(outcome) => Ended::Sent(outcome),
            Err(stop) => Ended::NotSent(stop),
        }
    }

    fn was_sent(end: &AutoMergeEnd) -> bool {
        matches!(end, Ended::Sent(_))
    }

    fn ended_changed(end: &AutoMergeEnd) -> bool {
        matches!(end, Ended::NotSent(stop) if stop.is_changed())
    }

    fn changed(&self, number: u64, what: &str) -> ActionRefusal {
        let action = match self.0 {
            AutoMergeChange::Enable(_) => PullRequestAction::EnableAutoMerge,
            AutoMergeChange::Disable => PullRequestAction::DisableAutoMerge,
        };
        ActionRefusal::Changed {
            action,
            pull_request: format!("#{number}"),
            what: what.to_owned(),
        }
    }

    /// Turning auto-merge on stands only for the stamp it was planned on; turning it off, the
    /// safe direction, stands while the number and the state hold, as the guard compares it, so
    /// a push or a retarget never withdraws it.
    fn still_stands(&self, seen: &PullRequestStamp, now: &PullRequestStamp) -> bool {
        match self.0 {
            AutoMergeChange::Enable(_) => seen == now,
            AutoMergeChange::Disable => seen.number == now.number && seen.state == now.state,
        }
    }

    fn plan(
        &self,
        service: &dyn PullRequestService,
        linked: &Linked,
        stamp: &PullRequestStamp,
        cancel: &Cancel,
    ) -> Result<ActionPlan, ActionStop> {
        service.plan_auto_merge(linked, stamp, self.0, cancel)
    }

    fn send(
        service: &dyn PullRequestService,
        linked: &Linked,
        load: &SendLoad<Self>,
        on_send: &(dyn Fn() + Sync),
        cancel: &Cancel,
    ) -> Result<AutoMergeOutcome, Stop<ActionRefusal>> {
        let ask = ActionAsk {
            stamp: &load.stamp,
            decision: Some(&load.decision),
            on_send,
        };
        service.set_auto_merge(linked, load.ask.0, ask, cancel)
    }

    fn attempt_log(flow: &AutoMergeFlow) -> String {
        auto_merge_attempt_log(flow)
    }

    fn outcome_log(flow: &AutoMergeFlow, end: &AutoMergeEnd) -> String {
        auto_merge_outcome_log(flow, end)
    }

    fn wrap(flow: AutoMergeFlow) -> TabFlow {
        TabFlow::AutoMerge(flow)
    }

    fn of(tab: &TabFlow) -> Option<&AutoMergeFlow> {
        match tab {
            TabFlow::AutoMerge(flow) => Some(flow),
            TabFlow::Merge(_) | TabFlow::Rerun(_) | TabFlow::DeleteBranch(_) => None,
        }
    }

    fn of_mut(tab: &mut TabFlow) -> Option<&mut AutoMergeFlow> {
        match tab {
            TabFlow::AutoMerge(flow) => Some(flow),
            TabFlow::Merge(_) | TabFlow::Rerun(_) | TabFlow::DeleteBranch(_) => None,
        }
    }
}
