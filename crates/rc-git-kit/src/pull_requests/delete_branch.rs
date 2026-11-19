//! Deleting a merged pull request's head branch on GitHub from its tab: whether the control shows
//! ([`delete_branch_control`], decided here from the guard's own [`head_branch_blocker`] and
//! protected names, never in the app), and the delete as one kind of the tab's guarded flow
//! ([`DeleteBranchFlow`], [`super::flow`]). The guard reads the branch again, writes the backup ref
//! in the repository first, and asks for the branch's name typed; nothing here decides that.

use super::copy::{
    branch_attempt_log, branch_in_fork, branch_no_right, branch_outcome_log, branch_protected,
    branch_untypeable,
};
use super::flow::{ActionStop, Ended, Flow, SendLoad, TabAction, TabFlow};
use super::link::Linked;
use super::state::DetailState;
use super::{
    ActionBlocker, ActionRefusal, BranchDeletePlan, BranchDeleteReport, Cancel, Decision,
    PullRequestAction, PullRequestService, PullRequestStamp, head_branch_blocker,
};
use crate::repository_actions::policy;

/// A head branch delete as the user asked for it: in the repository at `repo_path`, whose
/// backup ref keeps the branch's commit, read through the remote named `remote`, which the
/// outcome's note names to push the backup back.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeleteBranchIntent {
    /// The repository's tab path, where the backup ref is written.
    pub repo_path: String,
    /// The remote the pull requests are read through, e.g. `origin`.
    pub remote: String,
}

/// One head branch delete of one tab.
pub type DeleteBranchFlow = Flow<DeleteBranchIntent>;

/// How a head branch delete ended.
pub type DeleteBranchEnd = Ended<BranchDeleteReport, ActionRefusal>;

/// What the service is asked to send for a head branch delete, with the user's answer.
#[derive(Clone, Copy)]
pub struct BranchAsk<'a> {
    /// The repository's tab path, where the backup ref is written.
    pub repo_path: &'a str,
    /// What the user saw.
    pub stamp: &'a PullRequestStamp,
    /// The decision the challenge showed; `None` when the user did not confirm.
    pub decision: Option<&'a Decision>,
    /// What the user typed, untouched.
    pub typed: &'a str,
    /// Called once, just before the request leaves.
    pub on_send: &'a (dyn Fn() + Sync),
}

impl std::fmt::Debug for BranchAsk<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BranchAsk")
            .field("repo_path", &self.repo_path)
            .field("stamp", self.stamp)
            .field("decision", &self.decision)
            .field("typed", &self.typed)
            .finish_non_exhaustive()
    }
}

/// What the delete branch control shows for a tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteBranchControl {
    /// No control: nothing current is on screen, the account is signed out, or the pull request
    /// is not merged.
    Hidden,
    /// No button, and why: a fork's branch, a protected branch, no right GitHub reports, a name
    /// that cannot be typed. The guard would refuse each.
    Unavailable {
        /// Why, as the tab says it.
        reason: String,
    },
    /// Clickable.
    Shown {
        /// The branch's name, as the read shows it.
        branch: String,
    },
}

/// The delete branch control for `tab`, signed in when `linked`. Uses the guard's
/// [`head_branch_blocker`] and the policy's protected names, so the control and the delete refuse
/// alike; the guard decides again on its own read, which also refuses the default branch, a
/// branch someone pushed to since the merge, and a head commit not in the repository.
#[must_use]
pub fn delete_branch_control(tab: &DetailState, linked: bool) -> DeleteBranchControl {
    let (Some(_), Some(detail)) = (tab.stamp(), tab.detail()) else {
        return DeleteBranchControl::Hidden;
    };
    if !linked {
        return DeleteBranchControl::Hidden;
    }
    let branch = &detail.summary.head_ref;
    let reason = match head_branch_blocker(detail) {
        None if policy().is_protected(branch) => branch_protected(branch),
        None => {
            return DeleteBranchControl::Shown {
                branch: branch.clone(),
            };
        }
        Some(ActionBlocker::HeadNotInRepository) => branch_in_fork(branch),
        Some(ActionBlocker::NoDeleteRight) => branch_no_right(branch),
        Some(ActionBlocker::NameNotTypeable) => branch_untypeable(branch),
        // Not merged (an open pull request has no branch to delete yet), and anything else.
        Some(_) => return DeleteBranchControl::Hidden,
    };
    DeleteBranchControl::Unavailable { reason }
}

impl TabAction for DeleteBranchIntent {
    type Plan = BranchDeletePlan;
    type Outcome = BranchDeleteReport;
    type Refusal = ActionRefusal;
    type End = DeleteBranchEnd;

    fn decision(plan: &BranchDeletePlan) -> &Decision {
        &plan.decision
    }

    fn end(answer: Result<BranchDeleteReport, ActionStop>) -> DeleteBranchEnd {
        match answer {
            Ok(report) => Ended::Sent(report),
            Err(stop) => Ended::NotSent(stop),
        }
    }

    fn was_sent(end: &DeleteBranchEnd) -> bool {
        matches!(end, Ended::Sent(_))
    }

    fn ended_changed(end: &DeleteBranchEnd) -> bool {
        matches!(end, Ended::NotSent(stop) if stop.is_changed())
    }

    fn changed(&self, number: u64, what: &str) -> ActionRefusal {
        ActionRefusal::Changed {
            action: PullRequestAction::DeleteHeadBranch,
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
    ) -> Result<BranchDeletePlan, ActionStop> {
        service.plan_delete_branch(linked, &self.repo_path, stamp, cancel)
    }

    fn send(
        service: &dyn PullRequestService,
        linked: &Linked,
        load: &SendLoad<Self>,
        on_send: &(dyn Fn() + Sync),
        cancel: &Cancel,
    ) -> Result<BranchDeleteReport, ActionStop> {
        let ask = BranchAsk {
            repo_path: &load.ask.repo_path,
            stamp: &load.stamp,
            decision: Some(&load.decision),
            typed: &load.typed,
            on_send,
        };
        service.delete_branch(linked, ask, cancel)
    }

    fn attempt_log(flow: &DeleteBranchFlow) -> String {
        branch_attempt_log(flow)
    }

    fn outcome_log(flow: &DeleteBranchFlow, end: &DeleteBranchEnd) -> String {
        branch_outcome_log(flow, end)
    }

    fn wrap(flow: DeleteBranchFlow) -> TabFlow {
        TabFlow::DeleteBranch(flow)
    }

    fn of(tab: &TabFlow) -> Option<&DeleteBranchFlow> {
        match tab {
            TabFlow::DeleteBranch(flow) => Some(flow),
            TabFlow::Merge(_) | TabFlow::AutoMerge(_) | TabFlow::Rerun(_) => None,
        }
    }

    fn of_mut(tab: &mut TabFlow) -> Option<&mut DeleteBranchFlow> {
        match tab {
            TabFlow::DeleteBranch(flow) => Some(flow),
            TabFlow::Merge(_) | TabFlow::AutoMerge(_) | TabFlow::Rerun(_) => None,
        }
    }
}
