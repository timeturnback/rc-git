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

/// A parsed JSON answer and the response it came in, for its headers.
#[derive(Debug, Clone)]
pub(super) struct ApiReply {
    pub value: Value,
    response: Response,
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

    /// Runs the static GraphQL `query` with `variables` and answers its `data`. A response
    /// that carries `errors` is an error naming the first one.
    ///
    /// # Errors
    /// As [`Session::get`], and [`GitHubError::Status`] for GraphQL errors,
    /// [`GitHubError::Malformed`] for an answer without `data`.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the pull request reads call it")
    )]
    pub(super) fn graphql(
        &self,
        op: &'static str,
        query: &'static str,
        variables: &Value,
        cancel: &Cancel,
    ) -> GitHubResult<Value> {
        let url = self.host.graphql_url();
        let mut request = self.request(op, Method::Post, url);
        let body = serde_json::json!({ "query": query, "variables": variables });
        request.body = Some(Body::Json(body.to_string().into_bytes()));
        let shown_url = request.url.as_str().to_owned();
        let reply = self.run(request, cancel)?;
        if let Some(first) = reply
            .value
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|e| e.first())
        {
            let message = first
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Err(GitHubError::Status {
                op,
                url: shown_url,
                status: reply.response.status,
                message: server_text(message, &[self.token.expose_secret()]),
            });
        }
        match reply.value.get("data") {
            Some(data) if !data.is_null() => Ok(data.clone()),
            Some(_) | None => Err(GitHubError::Malformed {
                op,
                url: shown_url,
                reason: "the GraphQL answer has no data".to_owned(),
            }),
        }
    }

    /// Refuses a repository on another host than the session's.
    fn check_target(&self, target: &RepositoryTarget) -> GitHubResult<()> {
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
