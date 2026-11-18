//! Every way a GitHub call fails. Each message names the operation and the URL or host it
//! failed on, never a secret: a token, refresh token or device code is never a field, and every
//! text that came from the server or from curl passes through [`super::text::scrub`] with the
//! secrets the call held before it lands in one.

/// A GitHub sign-in, refresh or API call that failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitHubError {
    /// The caller's cancel flag was set before or during the call.
    #[error("{op}: cancelled")]
    Cancelled {
        /// What was being done.
        op: &'static str,
    },
    /// Text typed as a host is not one: a scheme, a path, userinfo, a bad label or port.
    #[error("'{input}' is not a GitHub host: {reason}")]
    InvalidHost {
        /// The input, made single-line.
        input: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A repository's remote URL that names no repository on the account's host. Nothing is
    /// sent for it.
    #[error(
        "remote '{remote}' {reason}; rc-git sends a GitHub token only to the host it was \
         issued for"
    )]
    ForeignRemote {
        /// The remote URL with any userinfo removed, made single-line.
        remote: String,
        /// Why it was refused.
        reason: String,
    },
    /// A value that must match a fixed shape before it enters a URL, a header or a request
    /// body: an owner or repository name, a client id, a login. Never a secret.
    #[error("{field} '{value}' is not valid: {reason}")]
    InvalidValue {
        /// Which value.
        field: &'static str,
        /// The value, made single-line.
        value: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A token or refresh token that cannot be sent as one. The value is never shown.
    #[error("the {what} is not valid: {reason}")]
    InvalidSecret {
        /// `token` or `refresh token`.
        what: &'static str,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The request never got an HTTP answer: DNS, connect, TLS, an idle or total timeout.
    #[error("{op} {url}: {reason}")]
    Transport {
        /// What was being done.
        op: &'static str,
        /// The URL asked.
        url: String,
        /// curl's description, scrubbed.
        reason: String,
    },
    /// The response body is over the call's byte cap. Nothing of it is parsed.
    #[error("{op} {url}: the response is over the {limit}-byte limit")]
    TooLarge {
        /// What was being done.
        op: &'static str,
        /// The URL asked.
        url: String,
        /// The cap.
        limit: usize,
    },
    /// The server answered with a redirect. rc-git follows none, so a token never reaches
    /// the target.
    #[error("{op} {url}: the server redirected (HTTP {status}); rc-git follows no redirect")]
    Redirected {
        /// What was being done.
        op: &'static str,
        /// The URL asked.
        url: String,
        /// The 3xx status.
        status: u32,
    },
    /// The host refused the token (HTTP 401).
    #[error("{op} {url}: {host} rejected the token (HTTP 401); sign in again")]
    Unauthorized {
        /// What was being done.
        op: &'static str,
        /// The URL asked.
        url: String,
        /// The host.
        host: String,
    },
    /// The host says the rate limit is spent.
    #[error("{op} {url}: rate limited by the host{}", reset_note(*.reset))]
    RateLimited {
        /// What was being done.
        op: &'static str,
        /// The URL asked.
        url: String,
        /// When the limit resets, in seconds since the Unix epoch, when the host said.
        reset: Option<u64>,
    },
    /// Any other non-success status, with GitHub's own message.
    #[error("{op} {url}: HTTP {status}: {message}")]
    Status {
        /// What was being done.
        op: &'static str,
        /// The URL asked.
        url: String,
        /// The status code.
        status: u32,
        /// GitHub's `message`, made single-line and scrubbed; empty when it sent none.
        message: String,
    },
    /// A success response whose body is not what the call expects.
    #[error("{op} {url}: unexpected response: {reason}")]
    Malformed {
        /// What was being done.
        op: &'static str,
        /// The URL asked.
        url: String,
        /// What was missing or wrong.
        reason: String,
    },
    /// The device flow named a verification page other than `https://<host>/login/device`.
    /// rc-git opens only its own page, so the flow stops.
    #[error(
        "sign in to {host}: the server named '{uri}' as the page to enter the code on, not \
         {expected}; refused"
    )]
    ForeignVerificationUri {
        /// The host signed into.
        host: String,
        /// What the server sent, made single-line.
        uri: String,
        /// The only page rc-git opens.
        expected: String,
    },
    /// Another device flow of this process is still waiting for its code.
    #[error(
        "sign in to {host}: another sign-in is still waiting for its code; finish or cancel it \
         first"
    )]
    FlowInProgress {
        /// The host this attempt was for.
        host: String,
    },
    /// The device code expired before the user entered it.
    #[error("sign in to {host}: the code expired before it was entered; start again")]
    DeviceCodeExpired {
        /// The host signed into.
        host: String,
    },
    /// The user declined the request on the host.
    #[error("sign in to {host}: the request was declined on {host}")]
    AccessDenied {
        /// The host signed into.
        host: String,
    },
    /// Any other OAuth error the sign-in endpoints sent.
    #[error("sign in to {host}: {code}: {description}")]
    OAuth {
        /// The host signed into.
        host: String,
        /// The `error` code, made single-line.
        code: String,
        /// The `error_description`, made single-line and scrubbed.
        description: String,
    },
    /// The host refused the refresh token: it was used already, expired, or revoked.
    #[error("refresh the {host} token: {host} refused the refresh token ({code}); sign in again")]
    RefreshRejected {
        /// The host.
        host: String,
        /// The `error` code, made single-line.
        code: String,
    },
}

fn reset_note(reset: Option<u64>) -> String {
    match reset {
        Some(reset) => format!("; it resets at {reset} (seconds since 1970-01-01 UTC)"),
        None => String::new(),
    }
}
