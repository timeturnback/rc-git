//! The GitHub API client: sign-in (GitHub App and OAuth App device flow, a pasted personal
//! access token), token refresh, and the authenticated REST and GraphQL primitives the pull
//! request features build on. It is the only code in rc-git that calls libcurl directly
//! (`clippy.toml` bans the `curl` entry points everywhere else), and it never follows a
//! repository: every URL it builds comes from a [`GitHubHost`] the user signed into, and a
//! repository's remote URL only ever selects the owner and name on that host
//! ([`RepositoryTarget::on_host`]). `docs/safety.md`, "GitHub API", has the rules.
//!
//! No curl or serde type crosses the public API. Secrets travel as [`ApiToken`] and
//! [`RefreshToken`], whose `Debug` is redacted and which have no `Display`.

mod api;
mod device;
mod error;
mod host;
mod http;
mod links;
mod oauth;
mod pull_types;
mod pulls;
pub(super) mod repository;
mod secret;
mod text;
mod user;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod pull_tests;

pub use device::{DeviceFlow, DeviceMethod, DevicePoll, start_device_flow};
pub use error::GitHubError;
pub use host::{DEV_ALLOWANCE, GitHubHost, RepositoryTarget, remote_host, ssh_dialed_host};
pub use links::ServerLink;
pub use oauth::refresh_user_token;
pub use pull_types::{
    ActionsIds, AutoMerge, Check, CheckConclusion, CheckList, CheckProgress, CheckStatus,
    DetailFields, MergeMethod, MergeMethods, MergeStateStatus, Mergeable, PageCursor,
    PullRequestDetail, PullRequestPage, PullRequestStamp, PullRequestState, PullRequestSummary,
    ReviewDecision, StatusState, ViewerAbilities,
};
pub use pulls::{MAX_CHECKS, PAGE_SIZE, list_open_pull_requests, read_pull_request};
pub use repository::{PullRequestRemote, pull_request_base_ref, pull_request_remote};
pub use secret::{ApiToken, ClientId, RefreshToken, TokenSet, unix_now};
pub use text::{multi_line, single_line};
pub use user::{TokenIdentity, validate_token};

/// The exact request bodies the pull request reads send, for tests in other crates that script
/// testkit's fake GitHub host, which answers only the body a call must send. Built only for tests
/// (the dev-only `test-support` feature), never into the app or the CLI.
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use super::{PageCursor, RepositoryTarget, api, pulls};

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
