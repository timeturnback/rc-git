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
mod oauth;
mod secret;
mod text;
mod user;

#[cfg(test)]
mod tests;

pub use device::{DeviceFlow, DeviceMethod, DevicePoll, start_device_flow};
pub use error::GitHubError;
pub use host::{GitHubHost, RepositoryTarget, remote_host};
pub use oauth::refresh_user_token;
pub use secret::{ApiToken, ClientId, RefreshToken, TokenSet, unix_now};
pub use text::single_line;
pub use user::{TokenIdentity, validate_token};

/// What a result of this module is.
pub type GitHubResult<T> = Result<T, GitHubError>;
