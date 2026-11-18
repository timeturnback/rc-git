//! Authenticated REST and GraphQL calls: one account's token, sent only to that account's host.
//! A [`Session`] pairs the two once, so no call can name another host for the token; a
//! repository-scoped call also checks that the [`RepositoryTarget`] is on that host. GraphQL
//! queries are `&'static str` and every value travels in `variables`, never in the query text.

use serde_json::Value;

use super::host::GitHubHost;
use super::http::{Body, Method, Request, Response, send};
use super::secret::ApiToken;
use super::text::server_text;
use super::{GitHubError, GitHubResult, RepositoryTarget};
use crate::actions::remote::Cancel;

/// The default cap on an API response body.
pub(super) const API_BODY_LIMIT: usize = 4 * 1024 * 1024;

/// The media type the REST API answers with.
const REST_ACCEPT: &str = "application/vnd.github+json";

/// A parsed JSON answer and the response it came in, for its headers. Its `Debug` shows the
/// body no more than [`Response`]'s does.
#[derive(Clone)]
pub(super) struct ApiReply {
    pub value: Value,
    response: Response,
}

impl std::fmt::Debug for ApiReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiReply")
            .field("value", &"<redacted>")
            .field("response", &self.response)
            .finish()
    }
}

impl ApiReply {
    /// The first value of header `name` (lowercase).
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.response.header(name)
    }
}

/// One account's token bound to that account's host.
#[derive(Debug, Clone, Copy)]
pub(super) struct Session<'a> {
    host: &'a GitHubHost,
    token: &'a ApiToken,
}

impl<'a> Session<'a> {
    /// `token`, which will only ever be sent to `host`.
    pub(super) fn new(host: &'a GitHubHost, token: &'a ApiToken) -> Self {
        Self { host, token }
    }

    fn request(&self, op: &'static str, method: Method, url: super::host::ApiUrl) -> Request {
        Request {
            op,
            method,
            url,
            accept: REST_ACCEPT,
            token: Some(self.token.clone()),
            api_version: true,
            body: None,
            limit: API_BODY_LIMIT,
            scrub: Vec::new(),
        }
    }

    /// `GET` the REST endpoint at `segments` (plain names) with the encoded `query`.
    ///
    /// # Errors
    /// What [`send`] and [`judge`] return, and [`GitHubError::InvalidValue`] for a segment that
    /// is not a plain name.
    pub(super) fn get(
        &self,
        op: &'static str,
        segments: &[&str],
        query: &str,
        cancel: &Cancel,
    ) -> GitHubResult<ApiReply> {
        let request = self.request(op, Method::Get, self.host.rest_url(segments, query)?);
        self.run(request, cancel)
    }

    /// `GET` a repository's REST endpoint: `repos/<owner>/<name>/<rest>`, refused before any
    /// request when `target` is on another host.
    ///
    /// # Errors
    /// [`GitHubError::ForeignRemote`] for a target on another host, else as [`Session::get`].
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the pull request reads call it")
    )]
    pub(super) fn get_repo(
        &self,
        op: &'static str,
        target: &RepositoryTarget,
        rest: &[&str],
        cancel: &Cancel,
    ) -> GitHubResult<ApiReply> {
        self.check_target(target)?;
        let mut segments = vec!["repos", target.owner(), target.name()];
        segments.extend_from_slice(rest);
        self.get(op, &segments, "", cancel)
    }

    /// `POST` `body` as JSON to the REST endpoint at `segments`.
    ///
    /// # Errors
    /// As [`Session::get`].
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the pull request mutations call it")
    )]
    pub(super) fn post(
        &self,
        op: &'static str,
        segments: &[&str],
        body: &Value,
        cancel: &Cancel,
    ) -> GitHubResult<ApiReply> {
        let mut request = self.request(op, Method::Post, self.host.rest_url(segments, "")?);
        request.body = Some(Body::Json(body.to_string().into_bytes()));
        self.run(request, cancel)
    }

    /// Runs the static GraphQL `query` with `variables`. The answer keeps its `data` and its
    /// `errors` apart, since GitHub answers a partly failed query with both: the caller decides
    /// what an error means for its read. A spent rate limit, which GitHub answers with a 200,
    /// `x-ratelimit-remaining: 0` and an error, or an error of type `RATE_LIMITED`, is
    /// [`GitHubError::RateLimited`].
    ///
    /// # Errors
    /// As [`Session::get`], and [`GitHubError::RateLimited`] as above.
    pub(super) fn graphql(
        &self,
        op: &'static str,
        query: &'static str,
        variables: &Value,
        cancel: &Cancel,
    ) -> GitHubResult<GraphqlReply> {
        let url = self.host.graphql_url();
        let mut request = self.request(op, Method::Post, url);
        request.body = Some(Body::Json(graphql_body(query, variables)));
        let shown_url = request.url.as_str().to_owned();
        let reply = self.run(request, cancel)?;
        let secrets = [self.token.expose_secret()];
        let errors: Vec<GraphqlError> = reply
            .value
            .get("errors")
            .and_then(Value::as_array)
            .map(|errors| {
                errors
                    .iter()
                    .map(|e| GraphqlError::read(e, &secrets))
                    .collect()
            })
            .unwrap_or_default();
        let spent = reply.header("x-ratelimit-remaining") == Some("0");
        let limited = errors
            .iter()
            .any(|e| e.kind.as_deref() == Some("RATE_LIMITED"));
        if limited || (spent && !errors.is_empty()) {
            return Err(GitHubError::RateLimited {
                op,
                url: shown_url,
                reset: reply
                    .header("x-ratelimit-reset")
                    .and_then(|reset| reset.parse().ok()),
            });
        }
        let data = reply
            .value
            .get("data")
            .filter(|data| !data.is_null())
            .cloned();
        Ok(GraphqlReply {
            data,
            errors,
            op,
            url: shown_url,
            status: reply.response.status,
        })
    }

    /// Refuses a repository on another host than the session's.
    pub(super) fn check_target(&self, target: &RepositoryTarget) -> GitHubResult<()> {
        if target.host() != self.host {
            return Err(GitHubError::ForeignRemote {
                remote: format!("{}/{}/{}", target.host(), target.owner(), target.name()),
                reason: format!("is not on '{}'", self.host),
            });
        }
        Ok(())
    }

    fn run(&self, request: Request, cancel: &Cancel) -> GitHubResult<ApiReply> {
        let op = request.op;
        let url = request.url.as_str().to_owned();
        let response = send(request, cancel)?;
        judge(response, op, &url, self.host, &[self.token.expose_secret()])
    }
}

/// The JSON body of a GraphQL request: the static query and the variables, nothing else.
pub(super) fn graphql_body(query: &'static str, variables: &Value) -> Vec<u8> {
    serde_json::json!({ "query": query, "variables": variables })
        .to_string()
        .into_bytes()
}

/// The longest GraphQL error code or type rc-git keeps.
const MAX_ERROR_WORD: usize = 64;

/// One entry of a GraphQL answer's `errors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GraphqlError {
    /// `message`, scrubbed and single-line.
    pub message: String,
    /// `extensions.code` (such as `undefinedField`) when it is a short plain word.
    pub code: Option<String>,
    /// `type` (such as `NOT_FOUND`) when it is a short plain word.
    pub kind: Option<String>,
}

impl GraphqlError {
    fn read(value: &Value, secrets: &[&str]) -> Self {
        let word = |value: Option<&Value>| {
            value
                .and_then(Value::as_str)
                .filter(|word| {
                    word.len() <= MAX_ERROR_WORD
                        && word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        && !secrets
                            .iter()
                            .any(|secret| !secret.is_empty() && word.contains(secret))
                })
                .map(str::to_owned)
        };
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Self {
            message: server_text(message, secrets),
            code: word(value.get("extensions").and_then(|e| e.get("code"))),
            kind: word(value.get("type")),
        }
    }

    /// Whether the error says the host's schema lacks a field or an argument the query names,
    /// as a GitHub Enterprise Server release older than the query does.
    pub(super) fn is_unknown_field(&self) -> bool {
        matches!(
            self.code.as_deref(),
            Some("undefinedField" | "argumentNotAccepted")
        )
    }
}

/// A GraphQL answer: its `data` (`None` when absent or null) and its `errors`.
#[derive(Debug, Clone)]
pub(super) struct GraphqlReply {
    pub data: Option<Value>,
    pub errors: Vec<GraphqlError>,
    op: &'static str,
    url: String,
    status: u32,
}

impl GraphqlReply {
    /// The answer as an error: the first GraphQL error's message, or `reason` when it sent
    /// none.
    pub(super) fn failure(&self, reason: &str) -> GitHubError {
        match self.errors.first() {
            Some(first) => GitHubError::Status {
                op: self.op,
                url: self.url.clone(),
                status: self.status,
                message: first.message.clone(),
            },
            None => self.malformed(reason),
        }
    }

    /// [`GitHubError::Malformed`] for this answer.
    pub(super) fn malformed(&self, reason: &str) -> GitHubError {
        GitHubError::Malformed {
            op: self.op,
            url: self.url.clone(),
            reason: reason.to_owned(),
        }
    }
}

/// An API response judged: a 2xx with a JSON body is the reply; 401 is
/// [`GitHubError::Unauthorized`]; a 403 or 429 with the rate limit spent is
/// [`GitHubError::RateLimited`]; any other status is [`GitHubError::Status`] with GitHub's
/// `message`, scrubbed of `secrets`.
///
/// # Errors
/// As above, and [`GitHubError::Malformed`] for a 2xx body that is not JSON.
pub(super) fn judge(
    response: Response,
    op: &'static str,
    url: &str,
    host: &GitHubHost,
    secrets: &[&str],
) -> GitHubResult<ApiReply> {
    let status = response.status;
    let parsed: Option<Value> = serde_json::from_slice(&response.body).ok();
    if (200..300).contains(&status) {
        let Some(value) = parsed else {
            return Err(GitHubError::Malformed {
                op,
                url: url.to_owned(),
                reason: "the body is not JSON".to_owned(),
            });
        };
        return Ok(ApiReply { value, response });
    }
    if status == 401 {
        return Err(GitHubError::Unauthorized {
            op,
            url: url.to_owned(),
            host: host.to_string(),
        });
    }
    let spent = response.header("x-ratelimit-remaining") == Some("0");
    if (status == 403 || status == 429) && (spent || response.header("retry-after").is_some()) {
        let reset = response
            .header("x-ratelimit-reset")
            .and_then(|reset| reset.parse().ok());
        return Err(GitHubError::RateLimited {
            op,
            url: url.to_owned(),
            reset,
        });
    }
    let message = parsed
        .as_ref()
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    Err(GitHubError::Status {
        op,
        url: url.to_owned(),
        status,
        message: server_text(message, secrets),
    })
}
