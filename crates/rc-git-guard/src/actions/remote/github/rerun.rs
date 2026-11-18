//! Re-running the failed jobs of a pull request's GitHub Actions runs: GitHub's REST
//! `POST /repos/{owner}/{repo}/actions/runs/{run_id}/rerun-failed-jobs`, once per run, through
//! the same proof as every other mutation. rc-git never calls the endpoint that approves a run
//! from a fork or a first-time contributor, and offers nothing that does.
//!
//! Which runs ([`failed_runs`]): the distinct workflow runs of the checks of the head commit the
//! user saw that are GitHub Actions jobs (`checkSuite.workflowRun.databaseId`) whose run finished
//! as failed, timed out or failed to start. A cancelled job, one waiting for an action, and a
//! check of another app are not counted; the challenge counts them apart. A run id is a
//! server-reported integer: only a positive one that fits GitHub's 64-bit ids becomes a
//! [`RunId`], and it only ever becomes a path segment. Runs go in ascending id order, at most
//! [`MAX_RERUN_RUNS`] per confirmation; the challenge says how many more there are and that they
//! are not included.
//!
//! The sequence, inside [`rerun_failed_jobs`]: a fresh read compared with the stamp, the
//! blocker ([`rerun_blocker`]: an open pull request with at least one failed run), the policy's
//! decision checked against the answered one (the run list and its check names are part of it,
//! so a check that moved since the challenge refuses), then one request per run in order. Each
//! is sent at most once; the outcome is per run ([`RunOutcome`]). After a run GitHub refused
//! with a plain `4xx`, the next run is still sent; after a cancel, a lost answer, a server
//! error, a refused token or a spent rate limit, no further run is sent and each left is
//! reported as not sent. An answer of `201 Created` is the only one read as requested; no read
//! settles anything, since nothing a read shows proves a re-run was asked for.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::api::{Answer, Mutated, Session};
use super::guarded::{
    ActionBlocker, ActionPlan, ActionRefusal, ActionTarget, PullRequestAction, PullRequestFacts,
    plan_action, proof_for,
};
use super::merge::{MergeBlocker, fact, fact_line, quoted};
use super::pull_types::{
    CheckConclusion, CheckProgress, CheckState, CheckStatus, PullRequestDetail, PullRequestState,
};
use super::{GitHubError, RepositoryTarget};
use crate::actions::remote::Cancel;
use crate::{Action, Decision, Policy};

/// The most workflow runs one confirmation re-runs.
pub const MAX_RERUN_RUNS: usize = 10;

/// The most check names a challenge quotes per run; the rest are counted.
const NAMED_CHECKS: usize = 5;

/// The longest check name a challenge quotes, in characters.
const CHECK_CHARS: usize = 80;

/// The body every re-run sends: debug logging stays off.
pub(super) fn rerun_body() -> Value {
    json!({ "enable_debug_logging": false })
}

/// A GitHub Actions workflow run's id, as the host reported it: a positive integer that fits a
/// signed 64-bit id (GitHub's REST `run_id` is an `int64`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunId(u64);

impl RunId {
    /// `id` as a run id: 1 to 2^63 − 1, else `None`.
    #[must_use]
    pub fn new(id: u64) -> Option<Self> {
        let fits = i64::try_from(id).is_ok_and(|id| id > 0);
        fits.then_some(Self(id))
    }

    /// The id's number.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One workflow run with failed jobs, and the names of its failed checks, sorted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FailedRun {
    /// The run.
    pub id: RunId,
    /// Its failed checks' names, single-line as read, sorted.
    pub checks: Vec<String>,
}

/// The failed GitHub Actions runs of a pull request's head commit, as a read reports them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FailedRuns {
    /// The runs, in ascending id order.
    pub runs: Vec<FailedRun>,
    /// Checks that failed and are not a failed job of a run rc-git can name: another app's
    /// checks, commit statuses, cancelled jobs, jobs waiting for an action, and jobs whose run id
    /// is not one rc-git accepts.
    pub other: usize,
    /// Checks of the head the read did not hold.
    pub unread: usize,
}

/// Whether `progress` is a job that failed in a way "re-run failed jobs" is for: it finished as
/// failed, timed out, or failed to start.
fn is_failed_job(progress: CheckProgress) -> bool {
    match progress {
        CheckProgress::Run {
            status: CheckStatus::Completed,
            conclusion:
                Some(
                    CheckConclusion::Failure
                    | CheckConclusion::TimedOut
                    | CheckConclusion::StartupFailure,
                ),
        } => true,
        CheckProgress::Run { .. } | CheckProgress::Status(_) => false,
    }
}

/// The failed GitHub Actions runs of `detail`'s head commit, as [`FailedRuns`] describes them.
#[must_use]
pub fn failed_runs(detail: &PullRequestDetail) -> FailedRuns {
    let checks = &detail.checks;
    let mut runs: BTreeMap<RunId, Vec<String>> = BTreeMap::new();
    let mut other = 0;
    for check in &checks.checks {
        let run = check
            .actions
            .and_then(|ids| RunId::new(ids.workflow_run))
            .filter(|_| is_failed_job(check.progress));
        match (run, check.progress.state()) {
            (Some(run), _) => runs.entry(run).or_default().push(check.name.clone()),
            (None, CheckState::Failing) => other += 1,
            (None, CheckState::Pending | CheckState::Passing | CheckState::Neutral) => {}
        }
    }
    let mut failed = Vec::new();
    for (id, mut names) in runs {
        names.sort();
        failed.push(FailedRun { id, checks: names });
    }
    FailedRuns {
        runs: failed,
        other,
        unread: checks.total.saturating_sub(checks.checks.len()),
    }
}

/// What stands in the way of re-running the failed jobs of `detail`'s head; `None` when nothing
/// does. The one rule the control and the re-run itself both follow: an open pull request, with
/// at least one failed run rc-git can name.
#[must_use]
pub fn rerun_blocker(detail: &PullRequestDetail) -> Option<ActionBlocker> {
    let state = detail.summary.state;
    if state != PullRequestState::Open {
        return Some(ActionBlocker::Merge(MergeBlocker::NotOpen(state)));
    }
    failed_runs(detail)
        .runs
        .is_empty()
        .then_some(ActionBlocker::NoFailedRuns)
}

/// A re-run as the user confirms it: the facts its challenge names, the runs it asks for (at
/// most [`MAX_RERUN_RUNS`], in ascending id order) with their failed checks, and what it leaves
/// out. Built only by [`PullRequestRerun::new`], from a read [`rerun_blocker`] passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestRerun {
    facts: PullRequestFacts,
    runs: Vec<FailedRun>,
    /// Failed runs past the cap, not re-run by this confirmation.
    left_out: usize,
    other: usize,
    unread: usize,
}

impl PullRequestRerun {
    /// The re-run of `detail`'s failed runs, as read from `target` with the token of account
    /// `login`.
    ///
    /// # Errors
    /// The [`ActionBlocker`] [`rerun_blocker`] finds.
    pub fn new(
        target: &RepositoryTarget,
        login: &str,
        detail: &PullRequestDetail,
    ) -> Result<Self, ActionBlocker> {
        if let Some(blocker) = rerun_blocker(detail) {
            return Err(blocker);
        }
        let mut failed = failed_runs(detail);
        let left_out = failed.runs.len().saturating_sub(MAX_RERUN_RUNS);
        failed.runs.truncate(MAX_RERUN_RUNS);
        Ok(Self {
            facts: PullRequestFacts::read(target, login, detail),
            runs: failed.runs,
            left_out,
            other: failed.other,
            unread: failed.unread,
        })
    }

    /// The repository.
    #[must_use]
    pub fn target(&self) -> &RepositoryTarget {
        self.facts.target()
    }

    /// The pull request's number.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.facts.number()
    }

    /// The head commit whose runs are re-run.
    #[must_use]
    pub fn head_oid(&self) -> rc_git_core::Oid {
        self.facts.head_oid()
    }

    /// The runs it asks GitHub to re-run, in the order it sends them.
    #[must_use]
    pub fn runs(&self) -> &[FailedRun] {
        &self.runs
    }

    /// `owner/name#number`.
    #[must_use]
    pub fn label(&self) -> String {
        self.facts.label()
    }

    /// The challenge's text: rc-git's own first line (what GitHub does with a re-run, as its
    /// documentation says), the repository to the head, the runs with their failed checks, what
    /// is left out, the fork line, and the title last.
    pub(crate) fn summary(&self) -> String {
        let mut lines = vec![
            "Re-run the failed jobs of these GitHub Actions runs. GitHub re-runs each run's \
             failed jobs, as it counts them, and the jobs that depend on them, at the commit and \
             ref of the original run and with the privileges of whoever first triggered it; \
             rc-git cannot stop them once they start."
                .to_owned(),
        ];
        lines.extend(self.facts.lines());
        lines.push(fact_line("Runs", &self.runs_phrase()));
        if let Some(left) = self.left_out_phrase() {
            lines.push(fact_line("Not included", &left));
        }
        if let Some(fork) = self.facts.fork() {
            lines.push(fact_line(
                "Fork",
                &format!(
                    "its head branch is in {}. GitHub decides which workflow files, secrets and \
                     permissions these runs use; rc-git approves no run that waits for approval",
                    fork.owner_phrase()
                ),
            ));
        }
        lines.push(self.facts.title_line());
        lines.join("\n")
    }

    /// `2 runs: run 11 (“build”, “test”); run 12 (“lint”)`.
    fn runs_phrase(&self) -> String {
        let mut shown = Vec::new();
        for run in &self.runs {
            shown.push(format!("run {} ({})", run.id, named(&run.checks)));
        }
        let count = match self.runs.len() {
            1 => "1 run".to_owned(),
            count => format!("{count} runs"),
        };
        format!("{count}: {}", shown.join("; "))
    }

    /// What the re-run leaves out, joined with `; `; `None` when it leaves nothing out.
    fn left_out_phrase(&self) -> Option<String> {
        let mut items = Vec::new();
        if self.left_out > 0 {
            items.push(format!(
                "{} more run(s) with failed jobs, past the {MAX_RERUN_RUNS} one confirmation \
                 re-runs (the lowest ids go first)",
                self.left_out
            ));
        }
        if self.other > 0 {
            items.push(format!(
                "{} other failed check(s) that are not a failed job of a GitHub Actions run \
                 rc-git can name",
                self.other
            ));
        }
        if self.unread > 0 {
            items.push(format!(
                "{} check(s) of the head the read did not hold",
                self.unread
            ));
        }
        (!items.is_empty()).then(|| items.join("; "))
    }
}

/// The names in curly quotes, `a, b, c, d, e and 3 more`: at most [`NAMED_CHECKS`] names, each a
/// challenge value cut to [`CHECK_CHARS`].
fn named(names: &[String]) -> String {
    let mut shown = Vec::new();
    for name in names.iter().take(NAMED_CHECKS) {
        shown.push(quoted(&fact(name, CHECK_CHARS)));
    }
    let text = shown.join(", ");
    match names.len().saturating_sub(NAMED_CHECKS) {
        0 => text,
        more => format!("{text} and {more} more"),
    }
}

/// A re-run to run.
#[derive(Clone, Copy)]
pub struct RerunRequest<'a> {
    /// Which pull request, by which account.
    pub who: ActionTarget<'a>,
    /// The decision the challenge showed, as [`plan_rerun_failed_jobs`] handed it out; `None`
    /// when the user did not confirm, which sends nothing.
    pub answer: Option<&'a Decision>,
    /// Called once, just before the first request leaves, after the last point a cancel stops
    /// all of them.
    pub on_send: &'a (dyn Fn() + Sync),
}

impl std::fmt::Debug for RerunRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RerunRequest")
            .field("who", &self.who)
            .field("answer", &self.answer)
            .finish_non_exhaustive()
    }
}

/// How one run's request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// GitHub answered `201 Created`: the re-run was asked for.
    Requested,
    /// GitHub answered and refused (a `4xx`: the run is unknown to it, cannot be re-run now, or
    /// the account may not; a refused token; a spent rate limit). Nothing was re-run.
    Refused {
        /// GitHub's reason, single-line and scrubbed.
        reason: String,
    },
    /// The request left and no usable answer came back (a server error, a lost connection, an
    /// answer that is not `201`): GitHub may or may not re-run it.
    Unknown {
        /// What went wrong with the answer.
        problem: String,
    },
    /// The request never left: a cancel, a failure to connect, or an earlier run's answer that
    /// stopped the rest.
    NotSent {
        /// Why.
        reason: String,
    },
}

/// One run and how its request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    /// The run.
    pub run: RunId,
    /// How its request ended.
    pub outcome: RunOutcome,
}

/// How a re-run ended: one report per confirmed run, in the order they were to be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerunReport {
    /// The runs.
    pub runs: Vec<RunReport>,
}

/// What the challenge must show for re-running the failed jobs of the pull request `who` names:
/// reads it now, refuses when it changed since the stamp or has no failed run, and answers the
/// policy's decision for that read. Sends nothing but the read.
///
/// # Errors
/// [`ActionRefusal::Read`], [`ActionRefusal::Cancelled`], [`ActionRefusal::Changed`],
/// [`ActionRefusal::Blocked`], and [`ActionRefusal::Denied`] when the policy refuses it.
pub fn plan_rerun_failed_jobs(
    policy: &Policy,
    who: &ActionTarget<'_>,
    cancel: &Cancel,
) -> Result<ActionPlan, ActionRefusal> {
    plan_action(
        policy,
        who,
        PullRequestAction::RerunFailedJobs,
        |detail| build(who, detail),
        cancel,
    )
}

fn build(who: &ActionTarget<'_>, detail: &PullRequestDetail) -> Result<Action, ActionBlocker> {
    let rerun = PullRequestRerun::new(who.target, who.login, detail)?;
    Ok(Action::RerunFailedJobs(Box::new(rerun)))
}

/// Re-runs the failed jobs `request` names, in the module's sequence: a fresh read compared with
/// the stamp, the blocker, the policy's own decision checked against the answered one, then one
/// request per run, each once, in order.
///
/// # Errors
/// An [`ActionRefusal`] whenever nothing was sent, the first request failing to leave included;
/// once a request may have left, the answer is a [`RerunReport`], never an error.
pub fn rerun_failed_jobs(
    policy: &Policy,
    request: RerunRequest<'_>,
    cancel: &Cancel,
) -> Result<RerunReport, ActionRefusal> {
    let who = &request.who;
    let action = PullRequestAction::RerunFailedJobs;
    let pull_request = who.pull_request();
    let proof = proof_for(
        policy,
        who,
        (action, request.answer),
        |detail| build(who, detail),
        cancel,
    )?;
    let Action::RerunFailedJobs(rerun) = proof.action() else {
        return Err(ActionRefusal::NotConfirmed {
            action,
            pull_request,
        });
    };
    let runs: Vec<RunId> = rerun.runs().iter().map(|run| run.id).collect();
    (request.on_send)();
    let not_sent = |source| match source {
        GitHubError::Cancelled { .. } => ActionRefusal::Cancelled {
            action,
            pull_request: pull_request.clone(),
        },
        source => ActionRefusal::NotSent {
            action,
            pull_request: pull_request.clone(),
            source,
        },
    };
    let mut outbox = Session::new(who.host, who.token)
        .mutate(proof)
        .map_err(not_sent)?;
    let mut reports = Vec::new();
    let mut stopped: Option<String> = None;
    for run in runs {
        if let Some(why) = &stopped {
            reports.push(RunReport {
                run,
                outcome: RunOutcome::NotSent {
                    reason: why.clone(),
                },
            });
            continue;
        }
        let sent = outbox.send_next(cancel);
        if reports.is_empty()
            && let Some(Mutated::NotSent(source)) = sent
        {
            return Err(not_sent(source));
        }
        let (outcome, stops) = run_outcome(sent);
        stopped = stops.then(|| format!("not sent after run {run}'s answer"));
        reports.push(RunReport { run, outcome });
    }
    Ok(RerunReport { runs: reports })
}

/// How one run's request ended, and whether the runs after it must not be sent.
fn run_outcome(sent: Option<Mutated>) -> (RunOutcome, bool) {
    let answer = match sent {
        None => {
            let reason = "the confirmed re-run names no request for it".to_owned();
            return (RunOutcome::NotSent { reason }, true);
        }
        Some(Mutated::NotSent(error)) => {
            return (
                RunOutcome::NotSent {
                    reason: error.to_string(),
                },
                true,
            );
        }
        Some(Mutated::Lost(error)) => {
            return (
                RunOutcome::Unknown {
                    problem: error.to_string(),
                },
                true,
            );
        }
        Some(Mutated::Answered(answer)) => answer,
    };
    match answer {
        Ok(Answer::Rest { status: 201 }) => (RunOutcome::Requested, false),
        Ok(Answer::Rest { status }) => (
            RunOutcome::Unknown {
                problem: format!("GitHub answered HTTP {status}, not 201 Created"),
            },
            true,
        ),
        Ok(Answer::Graphql(_)) => (
            RunOutcome::Unknown {
                problem: "GitHub answered a REST request with a GraphQL reply".to_owned(),
            },
            true,
        ),
        Err(error @ (GitHubError::Unauthorized { .. } | GitHubError::RateLimited { .. })) => (
            RunOutcome::Refused {
                reason: error.to_string(),
            },
            true,
        ),
        Err(error @ GitHubError::Status { status, .. }) if (400..500).contains(&status) => (
            RunOutcome::Refused {
                reason: error.to_string(),
            },
            false,
        ),
        Err(error) => (
            RunOutcome::Unknown {
                problem: error.to_string(),
            },
            true,
        ),
    }
}
