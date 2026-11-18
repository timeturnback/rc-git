//! The secrets a GitHub sign-in yields and the public client id it starts from. Each secret
//! type prints `<redacted>` under `Debug`, has no `Display`, and holds only visible ASCII, so it
//! can neither split a request header nor hide in a log line; reading its text takes an
//! explicit [`ApiToken::expose_secret`].

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use super::GitHubError;

/// The longest token rc-git accepts. GitHub's are well under 300 bytes.
const MAX_SECRET_BYTES: usize = 1024;

/// Checks a secret's text without ever showing it: non-empty, at most [`MAX_SECRET_BYTES`],
/// visible ASCII only (no space, no line break).
fn check_secret(text: &str, what: &'static str) -> Result<(), GitHubError> {
    let invalid = |reason| GitHubError::InvalidSecret { what, reason };
    if text.is_empty() {
        return Err(invalid("it is empty"));
    }
    if text.len() > MAX_SECRET_BYTES {
        return Err(invalid("it is longer than any GitHub token"));
    }
    if !text.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(invalid(
            "it holds a space, a line break or a character no GitHub token has",
        ));
    }
    Ok(())
}

/// A GitHub access token: a user access token from a device flow, or a pasted personal access
/// token. Sent only as the `Authorization` header to the host it was issued for.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiToken(String);

impl ApiToken {
    /// The token in `text`, exactly as given.
    ///
    /// # Errors
    /// [`GitHubError::InvalidSecret`] when it is empty, too long, or holds anything but visible
    /// ASCII. The message never shows the text.
    pub fn new(text: String) -> Result<Self, GitHubError> {
        check_secret(&text, "token")?;
        Ok(Self(text))
    }

    /// The token's text, for the keychain and the `Authorization` header only.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiToken(<redacted>)")
    }
}

/// A GitHub App refresh token. Single-use: exchanging it invalidates it and the access token
/// it came with.
#[derive(Clone, PartialEq, Eq)]
pub struct RefreshToken(String);

impl RefreshToken {
    /// The refresh token in `text`, exactly as given.
    ///
    /// # Errors
    /// [`GitHubError::InvalidSecret`] as for [`ApiToken::new`].
    pub fn new(text: String) -> Result<Self, GitHubError> {
        check_secret(&text, "refresh token")?;
        Ok(Self(text))
    }

    /// The refresh token's text, for the keychain and the refresh request only.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RefreshToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RefreshToken(<redacted>)")
    }
}

/// The device code of a running device flow: it lets whoever holds it collect the token, so
/// it is never logged or shown.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct DeviceCode(String);

impl DeviceCode {
    pub(super) fn new(text: String) -> Result<Self, GitHubError> {
        check_secret(&text, "device code")?;
        Ok(Self(text))
    }

    pub(super) fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceCode(<redacted>)")
    }
}

/// An app's client id: a public value naming the GitHub App or OAuth App that signs in. There
/// is no client secret anywhere in rc-git.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(String);

impl ClientId {
    /// The client id in `text`, surrounding whitespace trimmed: 1 to 128 of
    /// `[A-Za-z0-9._-]`.
    ///
    /// # Errors
    /// [`GitHubError::InvalidValue`] naming the text when it has another shape.
    pub fn parse(text: &str) -> Result<Self, GitHubError> {
        let text = text.trim();
        let plain = text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if text.is_empty() || text.len() > 128 || !plain {
            return Err(GitHubError::InvalidValue {
                field: "client id",
                value: super::single_line(text, 80),
                reason: "a client id is 1 to 128 letters, digits, '.', '_' or '-'",
            });
        }
        Ok(Self(text.to_owned()))
    }

    /// The client id's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a sign-in or a refresh yields: the access token and, for a GitHub App with expiring
/// tokens, when it expires and the refresh token that renews it. Times are seconds since the
/// Unix epoch, computed when the response arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSet {
    /// The token every API call sends.
    pub access: ApiToken,
    /// When `access` stops working; `None` when it does not expire (or the host did not say).
    pub access_expires_at: Option<u64>,
    /// The single-use token that renews `access`; `None` when there is none.
    pub refresh: Option<RefreshToken>,
    /// When `refresh` stops working.
    pub refresh_expires_at: Option<u64>,
}

impl TokenSet {
    /// A token that never expires and has no refresh token: a pasted personal access token, or
    /// an OAuth App token.
    #[must_use]
    pub fn lasting(access: ApiToken) -> Self {
        Self {
            access,
            access_expires_at: None,
            refresh: None,
            refresh_expires_at: None,
        }
    }
}

/// Seconds since the Unix epoch now; 0 on a clock set before 1970.
#[must_use]
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_secret_debug_is_redacted() {
        let access = ApiToken::new("ghu_access_secret".to_owned()).unwrap();
        let refresh = RefreshToken::new("ghr_refresh_secret".to_owned()).unwrap();
        let device = DeviceCode::new("device_secret".to_owned()).unwrap();
        let set = TokenSet {
            access,
            access_expires_at: Some(1),
            refresh: Some(refresh),
            refresh_expires_at: Some(2),
        };
        let printed = format!("{set:?} {device:?} {set:#?}");
        for secret in ["ghu_access_secret", "ghr_refresh_secret", "device_secret"] {
            assert!(!printed.contains(secret), "{printed}");
        }
        assert!(printed.contains("<redacted>"), "{printed}");
    }

    #[test]
    fn a_secret_that_could_split_a_header_is_refused_without_being_shown() {
        for bad in ["", "tok\r\nX-Evil: 1", "tok en", "tök", "tok\0"] {
            let error = ApiToken::new(bad.to_owned()).unwrap_err();
            let printed = format!("{error} {error:?}");
            assert!(
                matches!(error, GitHubError::InvalidSecret { .. }),
                "{printed}"
            );
            if !bad.is_empty() {
                assert!(!printed.contains(bad), "{printed}");
            }
        }
        let long = "a".repeat(MAX_SECRET_BYTES + 1);
        assert!(RefreshToken::new(long).is_err());
    }

    #[test]
    fn client_ids_are_plain_public_words() {
        assert_eq!(
            ClientId::parse(" Iv23li.ab_c-9 ").unwrap().as_str(),
            "Iv23li.ab_c-9"
        );
        for bad in ["", "a b", "a/b", "a&b=c", &"x".repeat(129)] {
            assert!(ClientId::parse(bad).is_err(), "{bad}");
        }
    }
}
