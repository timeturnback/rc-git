//! The OAuth token endpoint, `https://<host>/login/oauth/access_token`, that both a device flow
//! poll and a GitHub App token refresh talk to. Every parameter travels in a form body, never
//! in the URL, and no client secret exists: GitHub refreshes a device-flow token with the
//! client id alone.

use serde_json::Value;

use super::host::GitHubHost;
use super::http::{Body, Method, Request, send};
use super::secret::{ApiToken, ClientId, RefreshToken, TokenSet, unix_now};
use super::text::{form_body, scrub, server_text};
use super::{GitHubError, GitHubResult};
use crate::actions::remote::Cancel;

/// The cap on a sign-in endpoint's answer; GitHub's are a few hundred bytes.
pub(super) const OAUTH_BODY_LIMIT: usize = 64 * 1024;

/// The longest lifetime rc-git believes for a token or code, so a hostile number cannot
/// overflow an expiry: ten years.
const MAX_LIFETIME_SECS: u64 = 10 * 365 * 24 * 60 * 60;

/// What the token endpoint answered.
#[derive(Debug)]
pub(super) enum TokenAnswer {
    /// The tokens.
    Tokens(TokenSet),
    /// An OAuth error, with the `interval` a `slow_down` names.
    Error {
        code: String,
        description: String,
        interval: Option<u64>,
    },
}

/// A JSON answer of a sign-in endpoint and the URL it came from.
pub(super) struct FormAnswer {
    pub value: Value,
    pub url: String,
    /// The secrets the form carried, for scrubbing what the answer says.
    pub secrets: Vec<String>,
}

/// How an OAuth `error` code that is not one of GitHub's shows.
pub(super) const UNRECOGNIZED_CODE: &str = "unrecognized error code";

/// The longest OAuth `error` code shown as sent.
const MAX_CODE_BYTES: usize = 64;

impl FormAnswer {
    fn secret_refs(&self) -> Vec<&str> {
        self.secrets.iter().map(String::as_str).collect()
    }

    /// Server text from the answer, scrubbed and single-line.
    pub(super) fn text(&self, field: &str) -> String {
        let raw = self
            .value
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default();
        server_text(raw, &self.secret_refs())
    }

    /// The OAuth `error` code, when the answer carries one, as [`oauth_code`] shows it.
    pub(super) fn error_code(&self) -> Option<String> {
        let raw = self.value.get("error").and_then(Value::as_str)?;
        Some(oauth_code(raw, &self.secret_refs()))
    }
}

/// `raw` as an OAuth `error` code fit to show and match on: GitHub's codes are 1 to 64
/// lowercase letters and `_` (`authorization_pending`, `bad_refresh_token`), so anything else,
/// or a code holding one of `secrets`, is [`UNRECOGNIZED_CODE`].
fn oauth_code(raw: &str, secrets: &[&str]) -> String {
    let scrubbed = scrub(raw, secrets);
    let plain = !scrubbed.is_empty()
        && scrubbed.len() <= MAX_CODE_BYTES
        && scrubbed
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_');
    if plain {
        scrubbed
    } else {
        UNRECOGNIZED_CODE.to_owned()
    }
}

/// A form for a sign-in endpoint of the web host.
pub(super) struct FormPost<'a> {
    /// The endpoint, e.g. `/login/device/code`.
    pub path: &'static str,
    /// What is being done, for messages.
    pub op: &'static str,
    /// The fields.
    pub fields: &'a [(&'a str, &'a str)],
    /// The secret values among `fields`.
    pub secrets: Vec<String>,
}

/// Posts `post` to its sign-in endpoint on `host` and parses the JSON answer, which GitHub
/// sends with status 200 for OAuth errors too.
///
/// # Errors
/// What [`send`] returns, and [`GitHubError::Status`] for an answer that is not JSON.
pub(super) fn post_form(
    host: &GitHubHost,
    post: FormPost<'_>,
    cancel: &Cancel,
) -> GitHubResult<FormAnswer> {
    let FormPost {
        path,
        op,
        fields,
        secrets,
    } = post;
    let url = host.web_url(path);
    let shown_url = url.as_str().to_owned();
    let request = Request {
        op,
        method: Method::Post,
        url,
        accept: "application/json",
        token: None,
        api_version: false,
        body: Some(Body::Form(form_body(fields))),
        limit: OAUTH_BODY_LIMIT,
        scrub: secrets.clone(),
    };
    let response = send(request, cancel)?;
    let Ok(value) = serde_json::from_slice::<Value>(&response.body) else {
        let shown: Vec<&str> = secrets.iter().map(String::as_str).collect();
        return Err(GitHubError::Status {
            op,
            url: shown_url,
            status: response.status,
            message: server_text(&String::from_utf8_lossy(&response.body), &shown),
        });
    };
    Ok(FormAnswer {
        value,
        url: shown_url,
        secrets,
    })
}

/// Posts `form` to the host's token endpoint and reads the answer. `secrets` are the values
/// the form carries.
///
/// # Errors
/// What [`post_form`] returns, and [`GitHubError::Malformed`] for JSON that is neither tokens
/// nor an error.
pub(super) fn post_token_endpoint(
    host: &GitHubHost,
    op: &'static str,
    form: &[(&str, &str)],
    secrets: Vec<String>,
    cancel: &Cancel,
) -> GitHubResult<TokenAnswer> {
    let post = FormPost {
        path: "/login/oauth/access_token",
        op,
        fields: form,
        secrets,
    };
    let answer = post_form(host, post, cancel)?;
    let value = &answer.value;
    if let Some(code) = answer.error_code() {
        return Ok(TokenAnswer::Error {
            code,
            description: answer.text("error_description"),
            interval: value.get("interval").and_then(Value::as_u64),
        });
    }
    let tokens = token_set(value, unix_now()).map_err(|reason| GitHubError::Malformed {
        op,
        url: answer.url.clone(),
        reason: reason.to_owned(),
    })?;
    Ok(TokenAnswer::Tokens(tokens))
}

/// The tokens in a success answer, expiry counted from `now`.
fn token_set(value: &Value, now: u64) -> Result<TokenSet, &'static str> {
    let text = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_owned);
    let lifetime = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_u64)
            .map(|secs| now.saturating_add(secs.min(MAX_LIFETIME_SECS)))
    };
    let access = text("access_token").ok_or("it has neither an access_token nor an error")?;
    let access = ApiToken::new(access).map_err(|_| "its access_token is not a token")?;
    let refresh = match text("refresh_token") {
        Some(refresh) => {
            Some(RefreshToken::new(refresh).map_err(|_| "its refresh_token is not a token")?)
        }
        None => None,
    };
    Ok(TokenSet {
        access,
        access_expires_at: lifetime("expires_in"),
        refresh_expires_at: refresh.as_ref().and(lifetime("refresh_token_expires_in")),
        refresh,
    })
}

/// Exchanges a GitHub App's single-use `refresh` token for a new access token and a new
/// refresh token. The old pair stops working as soon as GitHub answers, so the caller must
/// store the result before anything else and must never run two refreshes of one account at
/// once.
///
/// # Errors
/// [`GitHubError::RefreshRejected`] when the host refuses the refresh token (used already,
/// expired, revoked), else what the token endpoint call returns.
pub fn refresh_user_token(
    host: &GitHubHost,
    client: &ClientId,
    refresh: &RefreshToken,
    cancel: &Cancel,
) -> GitHubResult<TokenSet> {
    let form = [
        ("client_id", client.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.expose_secret()),
    ];
    let secrets = vec![refresh.expose_secret().to_owned()];
    match post_token_endpoint(host, "refresh the token", &form, secrets, cancel)? {
        TokenAnswer::Tokens(tokens) => Ok(tokens),
        TokenAnswer::Error { code, .. } => Err(GitHubError::RefreshRejected {
            host: host.to_string(),
            code,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_counts_from_the_answer_and_never_overflows() {
        let value = serde_json::json!({
            "access_token": "ghu_a",
            "expires_in": 28800,
            "refresh_token": "ghr_b",
            "refresh_token_expires_in": u64::MAX,
        });
        let set = token_set(&value, 1000).unwrap();
        assert_eq!(set.access_expires_at, Some(1000 + 28800));
        assert_eq!(set.refresh_expires_at, Some(1000 + MAX_LIFETIME_SECS));
        assert_eq!(set.refresh.unwrap().expose_secret(), "ghr_b");
    }

    #[test]
    fn a_token_answer_without_expiry_is_lasting() {
        let set = token_set(&serde_json::json!({ "access_token": "gho_a" }), 5).unwrap();
        assert_eq!(
            set,
            TokenSet::lasting(ApiToken::new("gho_a".to_owned()).unwrap())
        );
    }

    #[test]
    fn only_githubs_shape_of_error_code_is_shown() {
        for code in [
            "authorization_pending",
            "bad_refresh_token",
            "slow_down",
            "x",
        ] {
            assert_eq!(oauth_code(code, &[]), code);
        }
        let too_long = "a".repeat(MAX_CODE_BYTES + 1);
        for bad in [
            "",
            "Bad-Code",
            "access denied",
            "slow_down\u{202e}",
            "\u{1b}[31mred",
            "code\n",
            too_long.as_str(),
        ] {
            assert_eq!(oauth_code(bad, &[]), UNRECOGNIZED_CODE, "{bad:?}");
        }
        assert_eq!(
            oauth_code("ghr_lowercase_secret", &["ghr_lowercase_secret"]),
            UNRECOGNIZED_CODE,
            "a code that is a secret of the request is never shown, even in the right shape"
        );
        assert_eq!(
            oauth_code("pending_ghr_lowercase_secret", &["ghr_lowercase_secret"]),
            UNRECOGNIZED_CODE
        );
    }

    #[test]
    fn an_answer_with_a_header_splitting_token_is_malformed() {
        let value = serde_json::json!({ "access_token": "a\r\nb" });
        assert!(token_set(&value, 0).is_err());
        assert!(token_set(&serde_json::json!({}), 0).is_err());
    }
}
