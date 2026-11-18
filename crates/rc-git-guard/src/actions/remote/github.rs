//! The GitHub API client: sign-in (GitHub App and OAuth App device flow, a pasted personal
//! access token), token refresh, and the authenticated REST and GraphQL primitives the pull
//! request features build on. It is the only code in rc-git that calls libcurl directly
//! (`clippy.toml` bans the `curl` entry points everywhere else), and it never follows a
//! repository: every URL it builds comes from a [`GitHubHost`] the user signed into, and a
//! repository's remote URL only ever selects the owner and name on that host
//! ([`RepositoryTarget::on_host`]). `docs/safety.md`, "GitHub API", has the rules.
//!
//! It also changes things on GitHub, each the same way: a fresh read compared with what the user
//! saw, then requests built from the [`crate::Confirmation`] alone, each sent once, whose
//! outcome is done, refused or unknown, never resent. It merges a pull request
//! ([`merge_pull_request`]), as a plain merge or as an admin merge past branch protection
//! ([`MergeKind`]); turns auto-merge on and off ([`set_auto_merge`]); and re-runs the failed jobs
//! of a pull request's GitHub Actions runs ([`rerun_failed_jobs`]).
//!
//! No curl or serde type crosses the public API. Secrets travel as [`ApiToken`] and
//! [`RefreshToken`], whose `Debug` is redacted and which have no `Display`.

mod api;
mod auto_merge;
mod device;
mod error;
mod guarded;
mod host;
mod http;
mod links;
mod merge;
mod mutation;
mod oauth;
mod pull_types;
mod pulls;
pub(super) mod repository;
mod rerun;
mod secret;
mod text;
mod user;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod pull_tests;

#[cfg(test)]
mod merge_tests;

#[cfg(test)]
mod admin_merge_tests;

#[cfg(test)]
mod auto_merge_tests;

#[cfg(test)]
mod rerun_tests;

pub use auto_merge::{
    AutoMergeAfter, AutoMergeChange, AutoMergeDisable, AutoMergeEnable, AutoMergeOutcome,
    AutoMergeRequest, disable_auto_merge_blocker, enable_auto_merge_blocker, plan_auto_merge,
    set_auto_merge,
};
pub use device::{DeviceFlow, DeviceMethod, DevicePoll, start_device_flow};
pub use error::GitHubError;
pub use guarded::{ActionBlocker, ActionPlan, ActionRefusal, ActionTarget, PullRequestAction};
pub use host::{DEV_ALLOWANCE, GitHubHost, RepositoryTarget, remote_host, ssh_dialed_host};
pub use links::ServerLink;
pub use merge::{
    AfterLoss, Bypassed, MergeAnswer, MergeBlocker, MergeKind, MergeOutcome, MergePlan,
    MergeRefusal, MergeRequest, MergeTarget, PullRequestAdminMerge, PullRequestMerge, Settled,
    admin_merge_blocker, merge_blocker, merge_pull_request, plan_merge_pull_request,
};
pub use oauth::refresh_user_token;
pub use pull_types::{
    ActionsIds, AutoMerge, Check, CheckConclusion, CheckList, CheckProgress, CheckState,
    CheckStatus, CheckTally, DetailFields, MergeMethod, MergeMethods, MergeStateStatus, Mergeable,
    PageCursor, PullRequestDetail, PullRequestId, PullRequestPage, PullRequestStamp,
    PullRequestState, PullRequestSummary, ReviewDecision, StatusState, ViewerAbilities,
};
pub use pulls::{MAX_CHECKS, PAGE_SIZE, list_open_pull_requests, read_pull_request};
pub use repository::{PullRequestRemote, pull_request_base_ref, pull_request_remote};
pub use rerun::{
    FailedRun, FailedRuns, MAX_RERUN_RUNS, PullRequestRerun, RerunReport, RerunRequest, RunId,
    RunOutcome, RunReport, failed_runs, plan_rerun_failed_jobs, rerun_blocker, rerun_failed_jobs,
};
pub use secret::{ApiToken, ClientId, RefreshToken, TokenSet, unix_now};
pub use text::{is_escaped_on_one_line, multi_line, single_line};
pub use user::{TokenIdentity, validate_token};

/// The exact request bodies the pull request reads send, for tests in other crates that script
/// testkit's fake GitHub host, which answers only the body a call must send. Built only for tests
/// (the dev-only `test-support` feature), never into the app or the CLI.
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use super::{
        MergeMethod, PageCursor, PullRequestId, RepositoryTarget, api, mutation, pulls, rerun,
    };

    /// The body of the list read of `target` from `after` on.
    #[must_use]
    pub fn list_request_body(target: &RepositoryTarget, after: Option<&PageCursor>) -> String {
        body(pulls::LIST, &pulls::list_variables(target, after))
    }

    /// The body of the detail read of pull request `number`: the full query, or with `basic`
    /// the one an older GitHub Enterprise Server is asked again with.
    #[must_use]
    pub fn detail_request_body(target: &RepositoryTarget, number: u64, basic: bool) -> String {
        let query = if basic {
            pulls::DETAIL_BASIC
        } else {
            pulls::DETAIL_FULL
        };
        body(query, &pulls::detail_variables(target, number))
    }

    /// The body of the merge of the pull request with node id `id`, expecting head `head`
    /// (40 hex digits), with `method`: the one mutation request rc-git sends for it.
    ///
    /// # Panics
    /// When `id` is not a node id or `head` not a full commit id.
    #[must_use]
    pub fn merge_request_body(id: &str, head: &str, method: MergeMethod) -> String {
        #[expect(clippy::expect_used, reason = "a test helper given fixed values")]
        let id = PullRequestId::parse(id).expect("a node id");
        #[expect(clippy::expect_used, reason = "a test helper given fixed values")]
        let head = rc_git_core::Oid::from_hex(head).expect("a full commit id");
        body(mutation::MERGE, &mutation::variables_of(&id, head, method))
    }

    /// The body of turning on auto-merge for the pull request with node id `id`, expecting head
    /// `head` (40 hex digits), with `method`.
    ///
    /// # Panics
    /// When `id` is not a node id or `head` not a full commit id.
    #[must_use]
    pub fn enable_auto_merge_request_body(id: &str, head: &str, method: MergeMethod) -> String {
        #[expect(clippy::expect_used, reason = "a test helper given fixed values")]
        let id = PullRequestId::parse(id).expect("a node id");
        #[expect(clippy::expect_used, reason = "a test helper given fixed values")]
        let head = rc_git_core::Oid::from_hex(head).expect("a full commit id");
        body(
            mutation::ENABLE_AUTO_MERGE,
            &mutation::variables_of(&id, head, method),
        )
    }

    /// The body of turning off auto-merge for the pull request with node id `id`.
    #[must_use]
    pub fn disable_auto_merge_request_body(id: &str) -> String {
        body(
            mutation::DISABLE_AUTO_MERGE,
            &serde_json::json!({ "id": id }),
        )
    }

    /// The body of every re-run of failed jobs.
    #[must_use]
    pub fn rerun_request_body() -> String {
        rerun::rerun_body().to_string()
    }

    /// The cursor the host handed out as `text`.
    #[must_use]
    pub fn cursor(text: &str) -> PageCursor {
        PageCursor(text.to_owned())
    }

    fn body(query: &'static str, variables: &serde_json::Value) -> String {
        String::from_utf8_lossy(&api::graphql_body(query, variables)).into_owned()
    }
}

/// What a result of this module is.
pub type GitHubResult<T> = Result<T, GitHubError>;
