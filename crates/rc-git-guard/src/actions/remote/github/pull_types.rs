//! What the pull request reads answer, in this crate's own types. Every text came from the
//! server and is attacker-controlled (anyone can open a pull request on a public repository),
//! so it arrives neutralized: one-line fields through [`super::single_line`], the description
//! through [`super::multi_line`], URLs as [`ServerLink`]s. A value of an enum GitHub may grow
//! reads as that enum's `Unknown`, never as an error.

use rc_git_core::Oid;

use super::links::ServerLink;

/// Where a pull request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PullRequestState {
    /// Open.
    Open,
    /// Closed without merging.
    Closed,
    /// Merged.
    Merged,
    /// A state this build does not know.
    Unknown,
}

/// The review decision branch protection asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReviewDecision {
    /// Approved.
    Approved,
    /// A reviewer asked for changes.
    ChangesRequested,
    /// A review is required and none approved yet.
    ReviewRequired,
    /// A decision this build does not know.
    Unknown,
}

/// A commit status, or the combined status of a commit's checks (GraphQL `StatusState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusState {
    /// Every check passed.
    Success,
    /// A check failed.
    Failure,
    /// A check errored.
    Error,
    /// A check is still running.
    Pending,
    /// A required status has not reported yet.
    Expected,
    /// A state this build does not know.
    Unknown,
}

/// Whether the pull request merges cleanly (GraphQL `MergeableState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mergeable {
    /// It merges without conflicts.
    Mergeable,
    /// It conflicts with its base.
    Conflicting,
    /// GitHub has not computed it yet.
    Unknown,
}

/// What stands between the pull request and a merge (GraphQL `MergeStateStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MergeStateStatus {
    /// The head branch is behind its base.
    Behind,
    /// Branch protection blocks the merge.
    Blocked,
    /// Mergeable, checks passing.
    Clean,
    /// The merge commit cannot be created cleanly.
    Dirty,
    /// A draft.
    Draft,
    /// Mergeable with passing commit status and pre-receive hooks.
    HasHooks,
    /// Not computed yet, or a state this build does not know.
    Unknown,
    /// Mergeable with a non-passing commit status.
    Unstable,
}

/// A way to merge a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MergeMethod {
    /// A merge commit.
    Merge,
    /// One squashed commit.
    Squash,
    /// The commits rebased onto the base.
    Rebase,
}

/// The merge methods the repository allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MergeMethods {
    /// A merge commit.
    pub merge: bool,
    /// Squash.
    pub squash: bool,
    /// Rebase.
    pub rebase: bool,
}

/// What the signed-in account may do to the pull request, as the host says.
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent permissions the host reports, one GraphQL field each"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ViewerAbilities {
    /// Merge past branch protection (`viewerCanMergeAsAdmin`).
    pub can_merge_as_admin: bool,
    /// Turn on auto-merge (`viewerCanEnableAutoMerge`).
    pub can_enable_auto_merge: bool,
    /// Turn off auto-merge (`viewerCanDisableAutoMerge`).
    pub can_disable_auto_merge: bool,
    /// Delete the head branch (`viewerCanDeleteHeadRef`).
    pub can_delete_head_ref: bool,
}

/// Auto-merge, when it is on for the pull request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AutoMerge {
    /// How it will merge, when the host named a method this build knows.
    pub method: Option<MergeMethod>,
    /// Who turned it on, single-line.
    pub enabled_by: Option<String>,
}

/// How far a check run got (GraphQL `CheckStatusState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckStatus {
    /// Waiting to run.
    Queued,
    /// Running.
    InProgress,
    /// Pending.
    Pending,
    /// Requested.
    Requested,
    /// Waiting on a deployment protection rule or an approval.
    Waiting,
    /// Done: the conclusion says how it ended.
    Completed,
    /// A status this build does not know.
    Unknown,
}

/// How a finished check run ended (GraphQL `CheckConclusionState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckConclusion {
    /// It needs someone to act.
    ActionRequired,
    /// Cancelled.
    Cancelled,
    /// Failed.
    Failure,
    /// Neither passed nor failed.
    Neutral,
    /// Skipped.
    Skipped,
    /// Marked stale by GitHub.
    Stale,
    /// It failed to start.
    StartupFailure,
    /// Passed.
    Success,
    /// It ran out of time.
    TimedOut,
    /// A conclusion this build does not know.
    Unknown,
}

/// Where a check's result came from and how far it got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckProgress {
    /// A check run (GitHub Actions or another app): its status and, once completed, its
    /// conclusion.
    Run {
        /// How far it got.
        status: CheckStatus,
        /// How it ended, once completed.
        conclusion: Option<CheckConclusion>,
    },
    /// A commit status a service posted.
    Status(StatusState),
}

/// How one check, or a pull request's checks together, stand: the one reading of a check's
/// progress that the tab, the CLI and a merge's challenge share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CheckState {
    /// Failed, errored, timed out, cancelled, or needs someone to act.
    Failing,
    /// Not finished.
    Pending,
    /// Passed.
    Passing,
    /// Finished without passing or failing: neutral, skipped, stale.
    Neutral,
}

impl CheckProgress {
    /// How this check stands. A conclusion this build does not know reads as failing.
    #[must_use]
    pub fn state(&self) -> CheckState {
        match self {
            Self::Run {
                status: CheckStatus::Completed,
                conclusion,
            } => match conclusion {
                Some(CheckConclusion::Success) => CheckState::Passing,
                Some(
                    CheckConclusion::Neutral | CheckConclusion::Skipped | CheckConclusion::Stale,
                ) => CheckState::Neutral,
                Some(
                    CheckConclusion::Failure
                    | CheckConclusion::TimedOut
                    | CheckConclusion::Cancelled
                    | CheckConclusion::ActionRequired
                    | CheckConclusion::StartupFailure
                    | CheckConclusion::Unknown,
                )
                | None => CheckState::Failing,
            },
            Self::Run {
                status:
                    CheckStatus::Queued
                    | CheckStatus::InProgress
                    | CheckStatus::Pending
                    | CheckStatus::Requested
                    | CheckStatus::Waiting
                    | CheckStatus::Unknown,
                ..
            } => CheckState::Pending,
            Self::Status(state) => state.check_state(),
        }
    }
}

impl StatusState {
    /// How a commit status, or a rollup, stands. A state this build does not know reads as
    /// failing.
    #[must_use]
    pub fn check_state(self) -> CheckState {
        match self {
            Self::Success => CheckState::Passing,
            Self::Pending | Self::Expected => CheckState::Pending,
            Self::Failure | Self::Error | Self::Unknown => CheckState::Failing,
        }
    }
}

/// A head commit's checks counted by how they stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CheckTally {
    /// Passed.
    pub passing: usize,
    /// Failed.
    pub failing: usize,
    /// Not finished.
    pub pending: usize,
    /// Neutral, skipped or stale.
    pub neutral: usize,
    /// How many the commit has, counted or not: more than the four above together when the
    /// read did not hold them all.
    pub total: usize,
}

/// The GitHub Actions ids of a check run, for the job logs and the re-run of later slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActionsIds {
    /// The workflow run (`checkSuite.workflowRun.databaseId`).
    pub workflow_run: u64,
    /// The job: the check run's own `databaseId`, which is the Actions job id.
    pub job: u64,
}

/// One check of the head commit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Check {
    /// The check run's name or the status's context, single-line.
    pub name: String,
    /// How far it got.
    pub progress: CheckProgress,
    /// A status's description, single-line.
    pub description: Option<String>,
    /// Whether branch protection requires it for this pull request; `None` when the host
    /// did not say (an older GitHub Enterprise Server).
    pub required: Option<bool>,
    /// Its details page, never fetched.
    pub details: Option<ServerLink>,
    /// Set for a GitHub Actions job.
    pub actions: Option<ActionsIds>,
}

/// The head commit's checks: one page of at most [`super::MAX_CHECKS`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct CheckList {
    /// The checks read.
    pub checks: Vec<Check>,
    /// How many the commit has; more than `checks.len()` when the page did not hold them all.
    pub total: usize,
}

impl CheckList {
    /// The checks read, counted by how they stand.
    #[must_use]
    pub fn tally(&self) -> CheckTally {
        let mut tally = CheckTally {
            total: self.total.max(self.checks.len()),
            ..CheckTally::default()
        };
        for check in &self.checks {
            match check.progress.state() {
                CheckState::Passing => tally.passing += 1,
                CheckState::Failing => tally.failing += 1,
                CheckState::Pending => tally.pending += 1,
                CheckState::Neutral => tally.neutral += 1,
            }
        }
        tally
    }
}

/// One open pull request as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PullRequestSummary {
    /// Its number.
    pub number: u64,
    /// Its title, single-line.
    pub title: String,
    /// The author's login, single-line; `None` for a deleted account.
    pub author: Option<String>,
    /// The head branch's name, single-line.
    pub head_ref: String,
    /// The head commit.
    pub head_oid: Oid,
    /// The base branch's name, single-line.
    pub base_ref: String,
    /// Who owns the head repository; `None` when it was deleted.
    pub head_owner: Option<String>,
    /// Whether the head lives in another repository (a fork).
    pub cross_repository: bool,
    /// A draft.
    pub draft: bool,
    /// Open, closed or merged.
    pub state: PullRequestState,
    /// When it last changed, in seconds since the Unix epoch.
    pub updated_at: Option<i64>,
    /// The combined state of the head commit's checks; `None` when it has none.
    pub checks: Option<StatusState>,
    /// The review decision; `None` when the base needs no review.
    pub review: Option<ReviewDecision>,
}

/// Where the next page of pull requests starts, as the host handed it out: opaque text that
/// only ever travels back in a query's `variables`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PageCursor(pub(super) String);

/// The longest page cursor rc-git keeps.
const MAX_CURSOR_BYTES: usize = 256;

impl PageCursor {
    /// `text` as a cursor: 1 to 256 printable ASCII characters, else `None`.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let fits = !text.is_empty()
            && text.len() <= MAX_CURSOR_BYTES
            && text.bytes().all(|b| b.is_ascii_graphic());
        fits.then(|| Self(text.to_owned()))
    }

    /// The cursor's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A pull request's GraphQL node id (`PR_kwDO…`), as the host handed it out: opaque text that
/// only ever travels back in a mutation's `variables`, never shown and never part of a URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PullRequestId(String);

/// The longest node id rc-git keeps.
const MAX_NODE_ID_BYTES: usize = 256;

impl PullRequestId {
    /// `text` as a node id: 1 to 256 ASCII letters, digits, `_`, `-`, `=`, `+` or `/` (the
    /// alphabets of GitHub's current and legacy ids), else `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let fits = !text.is_empty()
            && text.len() <= MAX_NODE_ID_BYTES
            && text.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'=' | b'+' | b'/')
            });
        fits.then(|| Self(text.to_owned()))
    }

    /// The id's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A repository's GraphQL node id (`R_kgDO…`), as the host handed it out: opaque text that only
/// ever travels back in a mutation's `variables`, never shown and never part of a URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepositoryId(String);

impl RepositoryId {
    /// `text` as a node id, by [`PullRequestId::parse`]'s rule, else `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        PullRequestId::parse(text).map(|id| Self(id.0))
    }

    /// The id's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One page of open pull requests, most recently updated first.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PullRequestPage {
    /// The pull requests.
    pub pull_requests: Vec<PullRequestSummary>,
    /// Where the next page starts; `None` on the last page.
    pub next: Option<PageCursor>,
}

/// Which query a detail came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetailFields {
    /// Every field rc-git asks for.
    Full,
    /// The host's schema lacked a newer field (an older GitHub Enterprise Server), so the read
    /// left out the merge state, the viewer's abilities, auto-merge, required checks, the
    /// Actions ids and the base commit.
    Basic,
}

/// One pull request in full, for its tab.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PullRequestDetail {
    /// What the list shows.
    pub summary: PullRequestSummary,
    /// The node id a mutation names it by; `None` when the host sent none rc-git accepts,
    /// which leaves it unmergeable from rc-git.
    pub id: Option<PullRequestId>,
    /// The commit GitHub reports the base branch at (`baseRefOid`), the one its changes are
    /// compared with; `None` from a [`DetailFields::Basic`] read or an answer without a commit
    /// id.
    pub base_oid: Option<Oid>,
    /// The description, neutralized multi-line text; empty when there is none.
    pub body: String,
    /// The pull request's page on the host.
    pub url: Option<ServerLink>,
    /// Whether it merges cleanly.
    pub mergeable: Mergeable,
    /// What stands between it and a merge; `None` from a [`DetailFields::Basic`] read.
    pub merge_state: Option<MergeStateStatus>,
    /// The merge methods the repository allows.
    pub merge_methods: MergeMethods,
    /// Whether the repository allows auto-merge; `None` from a basic read.
    pub auto_merge_allowed: Option<bool>,
    /// What the signed-in account may do; `None` from a basic read.
    pub viewer: Option<ViewerAbilities>,
    /// Auto-merge, when it is on.
    pub auto_merge: Option<AutoMerge>,
    /// The head commit's checks.
    pub checks: CheckList,
    /// Which query answered.
    pub fields: DetailFields,
}

/// What the user saw of a pull request, for a later action to compare against a fresh read
/// before it changes anything ("changed since you looked").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PullRequestStamp {
    /// The pull request's number.
    pub number: u64,
    /// The head commit shown.
    pub head_oid: Oid,
    /// The base branch shown.
    pub base_ref: String,
    /// The state shown.
    pub state: PullRequestState,
}

impl PullRequestDetail {
    /// The stamp of this read.
    #[must_use]
    pub fn stamp(&self) -> PullRequestStamp {
        PullRequestStamp {
            number: self.summary.number,
            head_oid: self.summary.head_oid,
            base_ref: self.summary.base_ref.clone(),
            state: self.summary.state,
        }
    }
}
