//! A repository's GitHub pull requests, read-only: which account and repository the
//! repository's remote names ([`link`]), the list and the open pull request tabs as pure state
//! machines ([`state`]) driven by one model per window ([`model`]), how checks are summed up
//! ([`checks`]), when to poll ([`polling`]), which links in a description may open ([`body`]),
//! and the words ([`copy`]).
//!
//! The network reads are the guard's (`rc_git_guard::actions::github`), reached through the
//! [`PullRequestService`] seam: [`live::LivePullRequests`] in the app and the CLI, a fake in
//! tests. Every text the server sent is attacker-controlled and arrives neutralized by the
//! guard; nothing here fetches a URL or an image.

pub mod body;
pub mod checks;
pub mod copy;
pub mod link;
pub mod live;
pub mod model;
pub mod polling;
pub mod state;

pub use rc_git_guard::actions::Cancel;
pub use rc_git_guard::actions::github::{
    ActionsIds, AutoMerge, Check, CheckConclusion, CheckList, CheckProgress, CheckStatus,
    DetailFields, GitHubError, MergeMethod, MergeMethods, MergeStateStatus, Mergeable, PageCursor,
    PullRequestDetail, PullRequestPage, PullRequestRemote, PullRequestStamp, PullRequestState,
    PullRequestSummary, RepositoryTarget, ReviewDecision, ServerLink, StatusState, ViewerAbilities,
    multi_line, single_line,
};

use link::{Linked, RepositoryLink};

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
            | GitHubError::RefreshRejected { .. } => Self::Failed(error.to_string()),
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
}
