//! A repository's GitHub pull requests: which account and repository the repository's remote
//! names ([`link`]), the list and the open pull request tabs as pure state machines
//! ([`state`]) driven by one model per window ([`model`]), a pull request's changes read from
//! local objects after the user fetches its head ([`changes`]), how checks are summed up
//! ([`checks`]), when to poll ([`polling`]), which links in a description may open ([`body`]),
//! and the words ([`copy`]).
//!
//! A tab's guarded GitHub action is a state machine too ([`flow`]): planning, confirming,
//! sending, done. It is a merge ([`merge`]: plain, or an admin merge past branch protection where
//! GitHub says the account may), turning auto-merge on or off ([`auto_merge`]), re-running the
//! failed jobs of the head's GitHub Actions runs ([`rerun`]), or deleting a merged pull request's
//! head branch on GitHub after a backup ref keeps its commit locally ([`delete_branch`]); a tab
//! runs one at a time.
//!
//! A tab also reads a GitHub Actions check's job log on the user's click ([`logs`]): one read at a
//! time per tab, matched by the guard to the head on screen first, held in memory only.
//!
//! The network reads are the guard's (`rc_git_guard::actions::github`), and so are the writes:
//! the fetch of a pull request's head into `refs/rc-git/pr/<n>/head`, the backup ref a head
//! branch delete writes first, and the merge, the auto-merge change, the re-run and the branch
//! delete on GitHub, whose challenges the guard words from its own fresh read. All are reached
//! through the
//! [`PullRequestService`] seam: [`live::LivePullRequests`] in the app and the CLI, a
//! fake in tests. Every text the server sent is attacker-controlled and arrives neutralized by
//! the guard; nothing here fetches a URL or an image, and fetched commits are only diffed,
//! never checked out.

pub mod auto_merge;
pub mod body;
pub mod changes;
pub mod checks;
pub mod copy;
pub mod delete_branch;
pub mod flow;
pub mod link;
pub mod live;
pub mod logs;
pub mod merge;
pub mod model;
pub mod polling;
pub mod rerun;
pub mod state;

pub use rc_git_guard::Decision;
pub use rc_git_guard::actions::Cancel;
pub use rc_git_guard::actions::github::{
    ActionBlocker, ActionPlan, ActionRefusal, ActionsIds, AfterLoss, AutoMerge, AutoMergeAfter,
    AutoMergeChange, AutoMergeOutcome, Bypassed, Check, CheckConclusion, CheckList, CheckProgress,
    CheckStatus, CheckTally, DetailFields, FailedRun, FailedRuns, GitHubError, MAX_RERUN_RUNS,
    MergeBlocker, MergeKind, MergeMethod, MergeMethods, MergeOutcome, MergePlan, MergeRefusal,
    MergeStateStatus, Mergeable, PageCursor, PullRequestAction, PullRequestDetail, PullRequestId,
    PullRequestPage, PullRequestRemote, PullRequestStamp, PullRequestState, PullRequestSummary,
    RepositoryTarget, RerunReport, ReviewDecision, RunId, RunOutcome, RunReport, ServerLink,
    Settled, StatusState, ViewerAbilities, admin_merge_blocker, disable_auto_merge_blocker,
    enable_auto_merge_blocker, failed_runs, merge_blocker, multi_line, rerun_blocker, single_line,
};
pub use rc_git_guard::actions::github::{
    BranchAfter, BranchDeleteOutcome, BranchDeletePlan, BranchDeleteReport, BranchUse,
    head_branch_blocker, typeable,
};
pub use rc_git_guard::actions::github::{
    JobId, JobLog, LOG_BYTES, LOG_LINE_CHARS, LOG_LINES, LogCut,
};

use changes::{ChangesRead, FetchProblem, HeadFetched};
use delete_branch::BranchAsk;
use flow::{ActionAsk, ActionStop};
use link::{Linked, RepositoryLink};
use logs::{LogAsk, LogStop};
use merge::{MergeAsk, MergeStop, PlanAsk};
use rc_git_core::{BlobDiff, Oid, TreeChange};

use crate::platform::api_tokens::AccountKey;

/// Why a read gave no answer, in the terms the section and the tab show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The account has no usable token: it was signed out (here or by the CLI), the host
    /// refused the token, or an expired token could not be renewed. Polling stops; the user
    /// signs in again in Settings.
    SignedOut {
        /// The account (`login@host`), or the host that refused the token.
        account: String,
    },
    /// The host's rate limit is spent until `reset` (seconds since the Unix epoch), when it
    /// said. Polling waits until then.
    RateLimited {
        /// When the limit resets.
        reset: Option<u64>,
    },
    /// The read was stopped.
    Cancelled,
    /// Anything else: the host could not be reached, refused the read, or answered something
    /// rc-git cannot read. The cause, one line.
    Failed(String),
}

impl ReadError {
    /// The error a guard read failed with, in these terms.
    #[must_use]
    pub fn from_github(error: &GitHubError) -> Self {
        match error {
            GitHubError::Unauthorized { host, .. } => Self::SignedOut {
                account: host.clone(),
            },
            GitHubError::RateLimited { reset, .. } => Self::RateLimited { reset: *reset },
            GitHubError::Cancelled { .. } => Self::Cancelled,
            GitHubError::InvalidHost { .. }
            | GitHubError::ForeignRemote { .. }
            | GitHubError::InvalidValue { .. }
            | GitHubError::InvalidSecret { .. }
            | GitHubError::Transport { .. }
            | GitHubError::TooLarge { .. }
            | GitHubError::Redirected { .. }
            | GitHubError::Status { .. }
            | GitHubError::Malformed { .. }
            | GitHubError::ForeignVerificationUri { .. }
            | GitHubError::FlowInProgress { .. }
            | GitHubError::DeviceCodeExpired { .. }
            | GitHubError::AccessDenied { .. }
            | GitHubError::OAuth { .. }
            | GitHubError::RefreshRejected { .. }
            | GitHubError::LogDownloadRefused { .. }
            | GitHubError::LogDownload { .. } => Self::Failed(error.to_string()),
        }
    }
}

/// The seam between the models and the network: blocking calls, run off the UI thread.
pub trait PullRequestService: Send + Sync {
    /// Which repository and account the repository at `repo_path` (its tab path) reads pull
    /// requests from. Reads the repository's config, the app's account settings and the
    /// user's ssh config; no network.
    fn link(&self, repo_path: &str) -> RepositoryLink;

    /// One page of the linked repository's open pull requests, from `after` on.
    ///
    /// # Errors
    /// [`ReadError`] for every way the read fails.
    fn list(
        &self,
        linked: &Linked,
        after: Option<&PageCursor>,
        cancel: &Cancel,
    ) -> Result<PullRequestPage, ReadError>;

    /// Pull request `number` of the linked repository, in full.
    ///
    /// # Errors
    /// [`ReadError`] for every way the read fails.
    fn detail(
        &self,
        linked: &Linked,
        number: u64,
        cancel: &Cancel,
    ) -> Result<PullRequestDetail, ReadError>;

    /// Uses account `key` for the repository at `repo_path` from now on.
    ///
    /// # Errors
    /// The settings' failure, as a sentence.
    fn choose_account(&self, repo_path: &str, key: &AccountKey) -> Result<(), String>;

    /// What the repository at `repo_path` holds for the diff of the pull request `stamp`
    /// names against `base`, the commit GitHub reported its base branch at (the base branch's
    /// local copy only when the host reported none): whether both are local, their merge base
    /// and the changed files. Local only: no network.
    ///
    /// # Errors
    /// The read's failure, as a sentence.
    fn changes(
        &self,
        repo_path: &str,
        linked: &Linked,
        stamp: &PullRequestStamp,
        base: Option<Oid>,
    ) -> Result<ChangesRead, String>;

    /// One changed file's line diff.
    ///
    /// # Errors
    /// The read's failure, as a sentence.
    fn file_diff(&self, repo_path: &str, change: &TreeChange) -> Result<BlobDiff, String>;

    /// Fetches the head of the pull request `stamp` names into `refs/rc-git/pr/<n>/head`,
    /// through the guard, over the remote's own git transport and the git credentials saved
    /// for its host; the account's API token is never offered to it. Only when the user asked:
    /// nothing calls it on its own.
    ///
    /// # Errors
    /// [`FetchProblem`] for a cancel and every failure.
    fn fetch_head(
        &self,
        repo_path: &str,
        linked: &Linked,
        stamp: &PullRequestStamp,
        cancel: &Cancel,
    ) -> Result<HeadFetched, FetchProblem>;

    /// What merging the pull request `ask` names, with its method and as its kind of merge,
    /// needs the user to answer: the guard reads it again, refuses what changed since the stamp
    /// or cannot merge that way, and the policy words the challenge from that read. Sends no
    /// mutation.
    ///
    /// # Errors
    /// [`MergeStop`] for a cancel and every refusal.
    fn plan_merge(
        &self,
        linked: &Linked,
        ask: PlanAsk<'_>,
        cancel: &Cancel,
    ) -> Result<MergePlan, MergeStop>;

    /// Merges the pull request `ask` names through the guard, which reads it again, checks the
    /// answered decision against its own and sends the one mutation. Only when the user
    /// confirmed: nothing calls it on its own, and nothing calls it twice for one confirm.
    ///
    /// # Errors
    /// [`MergeStop`] whenever nothing was sent; once the request may have left, the answer is
    /// a [`MergeOutcome`].
    fn merge(
        &self,
        linked: &Linked,
        ask: MergeAsk<'_>,
        cancel: &Cancel,
    ) -> Result<MergeOutcome, MergeStop>;

    /// What changing auto-merge of the pull request `stamp` names as `change` says needs the user
    /// to answer: the guard reads it again, refuses what changed since the stamp or cannot be
    /// changed so, and the policy words the challenge from that read. Sends no mutation.
    ///
    /// # Errors
    /// [`ActionStop`] for a cancel and every refusal.
    fn plan_auto_merge(
        &self,
        linked: &Linked,
        stamp: &PullRequestStamp,
        change: AutoMergeChange,
        cancel: &Cancel,
    ) -> Result<ActionPlan, ActionStop>;

    /// Changes auto-merge as `change` says through the guard, which reads the pull request
    /// again, checks the answered decision against its own and sends the one mutation. Only when
    /// the user confirmed, never twice for one confirm.
    ///
    /// # Errors
    /// [`ActionStop`] whenever nothing was sent; once the request may have left, the answer is
    /// an [`AutoMergeOutcome`].
    fn set_auto_merge(
        &self,
        linked: &Linked,
        change: AutoMergeChange,
        ask: ActionAsk<'_>,
        cancel: &Cancel,
    ) -> Result<AutoMergeOutcome, ActionStop>;

    /// What re-running the failed jobs of the head of the pull request `stamp` names needs the
    /// user to answer, from the guard's fresh read. Sends no request but the read.
    ///
    /// # Errors
    /// [`ActionStop`] for a cancel and every refusal.
    fn plan_rerun(
        &self,
        linked: &Linked,
        stamp: &PullRequestStamp,
        cancel: &Cancel,
    ) -> Result<ActionPlan, ActionStop>;

    /// Re-runs the failed jobs through the guard: one request per confirmed run, each once.
    /// Only when the user confirmed, never twice for one confirm.
    ///
    /// # Errors
    /// [`ActionStop`] whenever nothing was sent; once a request may have left, the answer is a
    /// [`RerunReport`].
    fn rerun(
        &self,
        linked: &Linked,
        ask: ActionAsk<'_>,
        cancel: &Cancel,
    ) -> Result<RerunReport, ActionStop>;

    /// What deleting the head branch of the pull request `stamp` names needs the user to answer:
    /// the guard reads it and its branch again, refuses what changed since the stamp or cannot be
    /// deleted (the head commit missing from the repository at `repo_path` included), and the
    /// policy words the challenge. Sends no mutation and writes nothing.
    ///
    /// # Errors
    /// [`ActionStop`] for a cancel and every refusal.
    fn plan_delete_branch(
        &self,
        linked: &Linked,
        repo_path: &str,
        stamp: &PullRequestStamp,
        cancel: &Cancel,
    ) -> Result<BranchDeletePlan, ActionStop>;

    /// Deletes the head branch through the guard, which reads it again, checks the answered
    /// decision and the typed name, writes the backup ref in the repository at `ask.repo_path`,
    /// and only then sends the one mutation. Only when the user confirmed, never twice for one
    /// confirm.
    ///
    /// # Errors
    /// [`ActionStop`] whenever nothing was sent; once the request may have left, the answer is a
    /// [`BranchDeleteReport`].
    fn delete_branch(
        &self,
        linked: &Linked,
        ask: BranchAsk<'_>,
        cancel: &Cancel,
    ) -> Result<BranchDeleteReport, ActionStop>;

    /// The log of the GitHub Actions job `ask` names, through the guard: matched to `ask.head`
    /// first, then downloaded once, capped and neutralized. A read: only when the user asked,
    /// one at a time per tab.
    ///
    /// # Errors
    /// [`LogStop`] for a job that does not match or has not finished, and every failure.
    fn read_log(
        &self,
        linked: &Linked,
        ask: LogAsk<'_>,
        cancel: &Cancel,
    ) -> Result<JobLog, LogStop>;
}
