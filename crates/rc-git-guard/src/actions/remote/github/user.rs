//! Who a token signs in as: `GET /user` on the token's own host, with the scopes and expiry the
//! host reports beside the answer.

use serde_json::Value;

use super::api::Session;
use super::host::GitHubHost;
use super::secret::ApiToken;
use super::text::{is_plain_name, single_line};
use super::{GitHubError, GitHubResult};
use crate::actions::remote::Cancel;

/// What `GET /user` says about a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenIdentity {
    /// The account's login.
    pub login: String,
    /// `X-OAuth-Scopes` as sent (`repo, read:org`), single-line: an OAuth App token's or a
    /// classic personal access token's scopes. `None` when the host sent no such header, as
    /// for a GitHub App user token or a fine-grained token, whose permissions are the app's or
    /// the token's own.
    pub scopes: Option<String>,
    /// `GitHub-Authentication-Token-Expiration` as sent (`2026-12-01 10:00:00 UTC`),
    /// single-line: when a personal access token stops working. `None` when the host sent none.
    pub expires: Option<String>,
}

/// Asks `host` who `token` belongs to. The token goes to `host` alone.
///
/// # Errors
/// [`GitHubError::Unauthorized`] when the host refuses the token,
/// [`GitHubError::Malformed`] when the answer names no plain login, else what the request
/// returns.
pub fn validate_token(
    host: &GitHubHost,
    token: &ApiToken,
    cancel: &Cancel,
) -> GitHubResult<TokenIdentity> {
    const OP: &str = "check the token";
    let reply = Session::new(host, token).get(OP, &["user"], "", cancel)?;
    let login = reply
        .value
        .get("login")
        .and_then(Value::as_str)
        .filter(|login| is_plain_name(login) && login.len() <= 100)
        .ok_or_else(|| GitHubError::Malformed {
            op: OP,
            url: format!("{host} /user"),
            reason: "it names no login of letters, digits, '.', '_' or '-'".to_owned(),
        })?;
    let shown = |name: &str| reply.header(name).map(|value| single_line(value, 200));
    Ok(TokenIdentity {
        login: login.to_owned(),
        scopes: shown("x-oauth-scopes"),
        expires: shown("github-authentication-token-expiration"),
    })
}
