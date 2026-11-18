//! Re-running a pull request's failed GitHub Actions jobs from its tab: whether the control
//! shows ([`rerun_control`], decided here from the guard's own [`rerun_blocker`] and
//! [`failed_runs`], never in the app), and the re-run as one kind of the tab's guarded flow
//! ([`RerunFlow`], [`super::flow`]).

use super::copy::{rerun_attempt_log, rerun_outcome_log};
use super::flow::{ActionAsk, ActionStop, Ended, Flow, SendLoad, TabAction, TabFlow};
use super::link::Linked;
use super::state::DetailState;
use super::{
    ActionPlan, ActionRefusal, Cancel, Decision, MAX_RERUN_RUNS, PullRequestAction,
    PullRequestService, PullRequestStamp, RerunReport, failed_runs, rerun_blocker,
};

/// A re-run as the user asked for it: the failed jobs of the head the stamp names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RerunIntent;

/// One re-run of one tab.
pub type RerunFlow = Flow<RerunIntent>;

/// How a re-run ended.
pub type RerunEnd = Ended<RerunReport, ActionRefusal>;

/// What the re-run control shows for a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerunControl {
    /// No control: nothing current is on screen, the account is signed out, the pull request
    /// is not open, or no GitHub Actions run of its head has a failed job.
    Hidden,
    /// Clickable.
    Shown {
        /// How many runs one confirmation re-runs.
        runs: usize,
        /// How many more runs with failed jobs it leaves out (past [`MAX_RERUN_RUNS`]).
        left_out: usize,
    },
}

/// The re-run control for `tab`, signed in when `linked`. Uses the guard's [`rerun_blocker`],
/// so the control and the re-run itself refuse alike; the guard decides again on its own read.
#[must_use]
pub fn rerun_control(tab: &DetailState, linked: bool) -> RerunControl {
    let (Some(_), Some(detail)) = (tab.stamp(), tab.detail()) else {
        return RerunControl::Hidden;
    };
    if !linked || rerun_blocker(detail).is_some() {
        return RerunControl::Hidden;
    }
    let runs = failed_runs(detail).runs.len();
    RerunControl::Shown {
        runs: runs.min(MAX_RERUN_RUNS),
        left_out: runs.saturating_sub(MAX_RERUN_RUNS),
    }
}

impl TabAction for RerunIntent {
    type Plan = ActionPlan;
    type Outcome = RerunReport;
    type Refusal = ActionRefusal;
    type End = RerunEnd;

    fn decision(plan: &ActionPlan) -> &Decision {
        &plan.decision
    }

    fn end(answer: Result<RerunReport, ActionStop>) -> RerunEnd {
        match answer {
            Ok(report) => Ended::Sent(report),
            Err(stop) => Ended::NotSent(stop),
        }
    }

    fn was_sent(end: &RerunEnd) -> bool {
        matches!(end, Ended::Sent(_))
    }

    fn ended_changed(end: &RerunEnd) -> bool {
        matches!(end, Ended::NotSent(stop) if stop.is_changed())
    }

    fn changed(&self, number: u64, what: &str) -> ActionRefusal {
        ActionRefusal::Changed {
            action: PullRequestAction::RerunFailedJobs,
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
    ) -> Result<ActionPlan, ActionStop> {
        service.plan_rerun(linked, stamp, cancel)
    }

    fn send(
        service: &dyn PullRequestService,
        linked: &Linked,
        load: &SendLoad<Self>,
        on_send: &(dyn Fn() + Sync),
        cancel: &Cancel,
    ) -> Result<RerunReport, ActionStop> {
        let ask = ActionAsk {
            stamp: &load.stamp,
            decision: Some(&load.decision),
            on_send,
        };
        service.rerun(linked, ask, cancel)
    }

    fn attempt_log(flow: &RerunFlow) -> String {
        rerun_attempt_log(flow)
    }

    fn outcome_log(flow: &RerunFlow, end: &RerunEnd) -> String {
        rerun_outcome_log(flow, end)
    }

    fn wrap(flow: RerunFlow) -> TabFlow {
        TabFlow::Rerun(flow)
    }

    fn of(tab: &TabFlow) -> Option<&RerunFlow> {
        match tab {
            TabFlow::Rerun(flow) => Some(flow),
            TabFlow::Merge(_) | TabFlow::AutoMerge(_) => None,
        }
    }

    fn of_mut(tab: &mut TabFlow) -> Option<&mut RerunFlow> {
        match tab {
            TabFlow::Rerun(flow) => Some(flow),
            TabFlow::Merge(_) | TabFlow::AutoMerge(_) => None,
        }
    }
}
